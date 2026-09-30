//! Terminal progress (`OSC 9;4`) on a session's sidebar row and strip tab.
//!
//! Four treatments sit behind [`PROGRESS_STYLE`] while the design is chosen:
//! a ring in the mark's slot, a pie filling that slot, a hairline bar along
//! the bottom of the row or tab, and a tint filling the row from the left.
//! Ring and pie draw in the leading mark slot, where activity already lives;
//! bar and fill draw under the row's content and leave the mark alone. Each
//! has the same states: normal (the theme's ink), error (red), paused or
//! warning (amber), and indeterminate (a sweep that holds a still, dashed
//! pose under reduce motion).
//!
//! Like the activity mark, nothing here owns a clock: the caller hands in the
//! shared activity frame and repaints on its cadence.

use std::f32::consts::TAU;

use diri_proto::{AgentKind, SessionRecord, TerminalProgress, TerminalProgressState as State};
use diri_ui::{Ink, SemanticColors, StatusState};
use gpui::{
    AnyElement, Bounds, IntoElement, PathBuilder, Pixels, Point, Rgba, Window, canvas, div, point,
    prelude::*, px, relative,
};

/// How progress is drawn. See the module docs.
// Every treatment is kept until one is chosen; the rest are unused outside
// the screenshot tests.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ProgressStyle {
    Ring,
    Pie,
    Bar,
    Fill,
}

/// The shipped treatment.
pub(crate) const PROGRESS_STYLE: ProgressStyle = ProgressStyle::Ring;

impl ProgressStyle {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 4] = [Self::Ring, Self::Pie, Self::Bar, Self::Fill];

    #[cfg(test)]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Ring => "ring",
            Self::Pie => "pie",
            Self::Bar => "bar",
            Self::Fill => "fill",
        }
    }

    /// Whether the treatment takes the leading mark's slot rather than
    /// drawing under the row.
    pub(crate) fn in_mark(self) -> bool {
        matches!(self, Self::Ring | Self::Pie)
    }
}

/// The active treatment. Screenshot tests choose one with
/// `DIRI_VISUAL_PROGRESS=ring|pie|bar|fill`.
pub(crate) fn style() -> ProgressStyle {
    #[cfg(test)]
    if let Ok(name) = std::env::var("DIRI_VISUAL_PROGRESS")
        && let Some(style) = ProgressStyle::ALL
            .into_iter()
            .find(|style| style.name().eq_ignore_ascii_case(&name))
    {
        return style;
    }
    PROGRESS_STYLE
}

/// One progress indicator as a row or tab draws it. Part of their cached
/// props, so a report that did not move the percent re-renders nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ProgressFace {
    pub progress: TerminalProgress,
    /// The shared activity frame, 0–7. Zero unless the face animates.
    pub frame: usize,
    /// Reduce motion: an indeterminate face holds a still pose.
    pub still: bool,
}

impl ProgressFace {
    /// Whether this face needs the activity tick.
    pub(crate) fn animates(&self) -> bool {
        self.progress.state == State::Indeterminate && !self.still
    }
}

/// The progress a session's row shows, if any.
///
/// Agents such as Claude Code send `9;4;3` as their own "working" signal,
/// which the activity mark already shows, so an Agent's indeterminate report
/// is not drawn twice. Anything with a percent, and every report from a plain
/// terminal, is shown.
pub(crate) fn shown(session: &SessionRecord) -> Option<TerminalProgress> {
    let progress = session.terminal_progress?;
    let agent = session.effective_kind() != &AgentKind::SHELL;
    (!(agent && progress.state == State::Indeterminate)).then_some(progress)
}

pub(crate) fn face(session: &SessionRecord, frame: usize, still: bool) -> Option<ProgressFace> {
    shown(session).map(|progress| {
        let mut face = ProgressFace {
            progress,
            frame: 0,
            still,
        };
        if face.animates() {
            face.frame = frame % 8;
        }
        face
    })
}

fn ink(state: State, colors: SemanticColors) -> Rgba {
    match state {
        State::Error => Ink::on_surface(Ink::DANGER, colors),
        State::Paused => Ink::on_surface(Ink::ATTENTION, colors),
        State::Normal | State::Indeterminate | State::Unknown => colors.primary,
    }
}

/// The unfilled part of a ring or bar: the ink, barely there.
fn track(state: State, colors: SemanticColors) -> Rgba {
    let hue = ink(state, colors);
    match state {
        State::Error | State::Paused => hue.alpha(0.26),
        _ => hue.alpha(0.16),
    }
}

/// How much of the indicator is filled, 0–1.
fn filled(progress: TerminalProgress) -> f32 {
    f32::from(progress.percent.min(100)) / 100.0
}

/// Where the indeterminate sweep starts, 0–1, one lap per eight frames.
fn lap(face: &ProgressFace) -> f32 {
    (face.frame % 8) as f32 / 8.0
}

/// Ring or pie in the mark's slot. `inner` (a tab's agent logo) sits inside
/// the ring; the pie replaces it. `None` for the treatments that draw under
/// the row instead.
pub(crate) fn progress_mark(
    face: ProgressFace,
    colors: SemanticColors,
    inner: Option<AnyElement>,
) -> Option<AnyElement> {
    let style = style();
    if !style.in_mark() {
        return None;
    }
    let ring_around_logo = style == ProgressStyle::Ring && inner.is_some();
    let paint =
        move |bounds: Bounds<Pixels>, _: (), window: &mut Window, _: &mut gpui::App| match style {
            ProgressStyle::Ring => paint_ring(bounds, face, colors, ring_around_logo, window),
            _ => paint_pie(bounds, face, colors, window),
        };
    Some(
        div()
            .size(px(18.0))
            .flex_none()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(canvas(|_, _, _| (), paint).absolute().inset_0())
            .when(style == ProgressStyle::Ring, |slot| slot.children(inner))
            .into_any_element(),
    )
}

/// A sidebar row's leading mark: the progress ring or pie while a program
/// reports progress, else the activity mark. A question the session asks,
/// and sleep, still win: they are what the user has to act on.
pub(crate) fn leading_mark(
    state: StatusState,
    frame: usize,
    progress: Option<ProgressFace>,
    colors: SemanticColors,
) -> AnyElement {
    if !matches!(
        state,
        StatusState::NeedsInput { .. } | StatusState::Hibernated
    ) && let Some(mark) = progress.and_then(|face| progress_mark(face, colors, None))
    {
        return mark;
    }
    crate::session_presentation::activity_mark(state, frame, colors)
}

/// Bar or fill under a row or tab whose corners are `radius`, positioned
/// absolutely over its whole box. `None` for the mark treatments.
pub(crate) fn progress_underlay(
    face: ProgressFace,
    colors: SemanticColors,
    radius: f32,
) -> Option<AnyElement> {
    let state = face.progress.state;
    let hue = ink(state, colors);
    let indeterminate = state == State::Indeterminate;
    match style() {
        ProgressStyle::Bar => {
            // A 2 px hairline inset past the corners, so it never pokes out
            // of the rounded box, one pixel above its bottom edge.
            let lit = if indeterminate && face.still {
                dashes(hue.alpha(0.5), 4.0, 4.0)
            } else if indeterminate {
                let (left, width) = sweep_span(face);
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(left))
                    .w(relative(width))
                    .rounded(px(1.0))
                    .bg(hue.alpha(0.72))
            } else {
                div()
                    .h_full()
                    .w(relative(filled(face.progress)))
                    .rounded(px(1.0))
                    .bg(match state {
                        State::Normal | State::Unknown => hue.alpha(0.62),
                        _ => hue,
                    })
            };
            Some(
                div()
                    .absolute()
                    .left(px(radius))
                    .right(px(radius))
                    .bottom(px(1.0))
                    .h(px(2.0))
                    .rounded(px(1.0))
                    .overflow_hidden()
                    .bg(track(state, colors).alpha(0.10))
                    .child(lit)
                    .into_any_element(),
            )
        }
        ProgressStyle::Fill => {
            let tint = |alpha: f32| match state {
                State::Error | State::Paused => hue.alpha(alpha * 1.8),
                _ => hue.alpha(alpha),
            };
            let level = if indeterminate && face.still {
                dashes(tint(0.06), 8.0, 8.0)
            } else if indeterminate {
                let (left, width) = sweep_span(face);
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(left))
                    .w(relative(width))
                    .bg(tint(0.08))
            } else {
                // A brighter leading edge reads as a level, not a shadow.
                let percent = face.progress.percent;
                div()
                    .h_full()
                    .w(relative(filled(face.progress)))
                    .bg(tint(0.075))
                    .when(percent > 0 && percent < 100, |level| {
                        level.border_r_1().border_color(tint(0.16))
                    })
            };
            Some(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(radius))
                    .overflow_hidden()
                    .child(level)
                    .into_any_element(),
            )
        }
        ProgressStyle::Ring | ProgressStyle::Pie => None,
    }
}

/// A still indeterminate span: evenly spaced stripes, so it never reads as
/// a finished bar or a full row.
fn dashes(color: Rgba, dash: f32, gap: f32) -> gpui::Div {
    div()
        .size_full()
        .overflow_hidden()
        .flex()
        .gap(px(gap))
        .children((0..64).map(|_| div().flex_none().w(px(dash)).h_full().bg(color)))
}

/// The indeterminate band's left edge and width along a bar or row: it enters
/// from the left, crosses, and leaves on the right once a lap. Frames sit at
/// the middle of their eighth, so no frame is empty.
fn sweep_span(face: ProgressFace) -> (f32, f32) {
    const WIDTH: f32 = 0.34;
    ((lap(&face) + 1.0 / 16.0) * (1.0 + WIDTH) - WIDTH, WIDTH)
}

/// A point `turn` of the way clockwise around a circle from 12 o'clock.
fn on_circle(center: Point<Pixels>, radius: f32, turn: f32) -> Point<Pixels> {
    let angle = turn * TAU;
    point(
        center.x + px(radius * angle.sin()),
        center.y - px(radius * angle.cos()),
    )
}

/// Adds a clockwise arc from `start` for `turns` of a lap, in quarter-lap
/// pieces so a full circle is still well formed.
fn arc(path: &mut PathBuilder, center: Point<Pixels>, radius: f32, start: f32, turns: f32) {
    path.move_to(on_circle(center, radius, start));
    let pieces = (turns * 4.0).ceil().max(1.0) as usize;
    for piece in 1..=pieces {
        let at = start + turns * piece as f32 / pieces as f32;
        path.arc_to(
            point(px(radius), px(radius)),
            px(0.0),
            false,
            true,
            on_circle(center, radius, at),
        );
    }
}

fn stroke_arc(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    width: f32,
    (start, turns): (f32, f32),
    dashes: Option<f32>,
    color: Rgba,
) {
    if turns <= 0.0 {
        return;
    }
    let mut path = PathBuilder::stroke(px(width));
    if let Some(dash) = dashes {
        path = path.dash_array(&[px(dash), px(dash)]);
    }
    arc(&mut path, center, radius, start, turns);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
    // Round caps, which the stroke options cannot name from here.
    if dashes.is_none() && turns < 1.0 {
        for turn in [start, start + turns] {
            dot(window, on_circle(center, radius, turn), width / 2.0, color);
        }
    }
}

fn dot(window: &mut Window, center: Point<Pixels>, radius: f32, color: Rgba) {
    let mut path = PathBuilder::fill();
    arc(&mut path, center, radius, 0.0, 1.0);
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn paint_ring(
    bounds: Bounds<Pixels>,
    face: ProgressFace,
    colors: SemanticColors,
    around_logo: bool,
    window: &mut Window,
) {
    let state = face.progress.state;
    let center = bounds.center();
    // Alone, the ring matches the 14 px activity mark; around a tab's logo
    // it takes the whole slot so the logo keeps room to read.
    let (diameter, width) = if around_logo {
        (17.0, 1.5)
    } else {
        (13.5, 1.75)
    };
    let radius = (diameter - width) / 2.0;
    let hue = ink(state, colors);
    if state == State::Indeterminate && face.still {
        stroke_arc(
            window,
            center,
            radius,
            width,
            (0.0, 1.0),
            Some(radius * TAU / 12.0),
            hue.alpha(0.7),
        );
        return;
    }
    stroke_arc(
        window,
        center,
        radius,
        width,
        (0.0, 1.0),
        None,
        track(state, colors),
    );
    let span = if state == State::Indeterminate {
        (lap(&face), 0.28)
    } else {
        (0.0, filled(face.progress))
    };
    stroke_arc(window, center, radius, width, span, None, hue);
}

fn paint_pie(
    bounds: Bounds<Pixels>,
    face: ProgressFace,
    colors: SemanticColors,
    window: &mut Window,
) {
    let state = face.progress.state;
    let center = bounds.center();
    let hue = ink(state, colors);
    // An outline with a gap, then the wedge: macOS's download pie.
    let outline = 6.25;
    let wedge = 4.5;
    let still = state == State::Indeterminate && face.still;
    stroke_arc(
        window,
        center,
        outline,
        1.25,
        (0.0, 1.0),
        still.then_some(outline * TAU / 12.0),
        hue.alpha(if still { 0.7 } else { 0.55 }),
    );
    let (start, turns) = match state {
        State::Indeterminate if face.still => return dot(window, center, 1.5, hue.alpha(0.7)),
        State::Indeterminate => (lap(&face), 0.25),
        _ => (0.0, filled(face.progress)),
    };
    if turns <= 0.0 {
        return;
    }
    let mut path = PathBuilder::fill();
    if turns >= 1.0 {
        arc(&mut path, center, wedge, 0.0, 1.0);
    } else {
        path.move_to(center);
        path.line_to(on_circle(center, wedge, start));
        let pieces = (turns * 4.0).ceil().max(1.0) as usize;
        for piece in 1..=pieces {
            let at = start + turns * piece as f32 / pieces as f32;
            path.arc_to(
                point(px(wedge), px(wedge)),
                px(0.0),
                false,
                true,
                on_circle(center, wedge, at),
            );
        }
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, hue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(kind: AgentKind, state: State) -> SessionRecord {
        use crate::sidebar::{PreviewScenario, SidebarPreviewFixture};
        let mut session = SidebarPreviewFixture::make(PreviewScenario::Typical)
            .list
            .sessions
            .into_iter()
            .find(|session| session.kind == AgentKind::SHELL)
            .expect("shell fixture");
        session.kind = kind;
        session.terminal_progress = Some(TerminalProgress { state, percent: 40 });
        session
    }

    #[test]
    fn an_agents_own_working_signal_is_not_drawn_twice() {
        assert!(shown(&session(AgentKind::SHELL, State::Indeterminate)).is_some());
        assert!(shown(&session(AgentKind::CLAUDE_CODE, State::Indeterminate)).is_none());
        assert!(shown(&session(AgentKind::CLAUDE_CODE, State::Normal)).is_some());
        // Claude typed at a shell prompt is an Agent for this purpose.
        let mut typed = session(AgentKind::SHELL, State::Indeterminate);
        typed.foreground_agent = Some(AgentKind::CLAUDE_CODE);
        assert!(shown(&typed).is_none());
    }

    #[test]
    fn only_a_moving_sweep_reads_the_frame() {
        let shell = session(AgentKind::SHELL, State::Normal);
        assert_eq!(face(&shell, 5, false).unwrap().frame, 0);
        let busy = session(AgentKind::SHELL, State::Indeterminate);
        let moving = face(&busy, 13, false).unwrap();
        assert_eq!(moving.frame, 5);
        assert!(moving.animates());
        let still = face(&busy, 5, true).unwrap();
        assert_eq!(still.frame, 0);
        assert!(!still.animates());
    }

    #[test]
    fn the_sweep_enters_and_leaves_the_bar() {
        let busy = session(AgentKind::SHELL, State::Indeterminate);
        let first = sweep_span(face(&busy, 0, false).unwrap());
        assert!(first.0 < 0.0 && first.0 + first.1 > 0.0, "entering");
        let last = sweep_span(face(&busy, 7, false).unwrap());
        assert!(last.0 < 1.0 && last.0 + last.1 > 1.0, "leaving");
    }
}
