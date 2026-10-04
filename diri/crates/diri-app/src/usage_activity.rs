// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//! Settings › Usage activity: a calendar heatmap of how much you work each
//! day, and a weekday × hour heatmap of when. Ported from Ely's
//! `CalendarHeatmap` and `HeatmapChart`: painted cells on one canvas,
//! absolute labels, a pointer-picked tooltip, and a Less–More key.
//!
//! Bins come from `diri_usage::transcripts::activity` and are cached per
//! snapshot, source, and week start; a frame only paints quads.
use super::usage_page::usage_control;
use super::*;
use crate::usage::UsageFormat;
use crate::usage::activity::{
    self, Activity, ActivityDay, ActivityMetric, CALENDAR_WEEKS, IntensityScale,
};
use gpui::{MouseMoveEvent, fill, size};
use std::cell::RefCell;

/// Cell pitch and painted side, as in GitHub's contribution graph.
const PITCH: f32 = 14.0;
const CELL: f32 = 11.0;
const CELL_RADIUS: f32 = 2.5;
/// Room for weekday names on the left and month or hour names on top.
const GUTTER: f32 = 30.0;
const TOP: f32 = 18.0;
const GRID_H: f32 = TOP + PITCH * 6.0 + CELL;

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const PROVIDERS: [&str; 3] = ["Claude Code", "Codex", "Cursor"];

/// The part of the activity charts under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivityHover {
    /// Index into `Activity::days`.
    Day(usize),
    /// Calendar row × 24 + local hour.
    Slot(usize),
}

#[derive(Clone, Debug, PartialEq)]
struct ActivityKey {
    history: usize,
    updated_at: i64,
    now: i64,
    host: Option<String>,
    remote: Vec<(usize, i64)>,
    week_start: u8,
}

/// Bins and their strength scales for one snapshot and source.
pub(super) struct ActivityCache {
    key: ActivityKey,
    activity: Rc<Activity>,
    scales: Rc<[IntensityScale; 3]>,
}

pub(super) type ActivityCell = RefCell<Option<ActivityCache>>;

fn metric_index(metric: ActivityMetric) -> usize {
    match metric {
        ActivityMetric::Hours => 0,
        ActivityMetric::Tokens => 1,
        ActivityMetric::Cost => 2,
    }
}

/// The weekday rows start on: the system calendar's first weekday on macOS,
/// Monday elsewhere. Monday-zero.
pub(super) fn system_week_start() -> u8 {
    #[cfg(target_os = "macos")]
    {
        // NSCalendar counts weekdays from Sunday = 1.
        let first = objc2_foundation::NSCalendar::autoupdatingCurrentCalendar().firstWeekday();
        ((first + 5) % 7) as u8
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// Five steps from none to most of one ink, as Ely's calendar key.
fn steps(colors: SemanticColors) -> [Rgba; 5] {
    let ink = Ink::on_surface(Ink::FRESH, colors);
    [
        colors.primary.alpha(0.06),
        ink.alpha(0.3),
        ink.alpha(0.52),
        ink.alpha(0.76),
        ink,
    ]
}

/// Where calendar cell `index` sits, relative to the chart's origin.
pub(super) fn day_origin(index: usize) -> (f32, f32) {
    let (week, row) = Activity::place(index);
    (GUTTER + PITCH * week as f32, TOP + PITCH * row as f32)
}

/// The calendar cell under a point, if it is one of `days` cells. The gaps
/// between cells belong to the cell before them so the tooltip never
/// flickers off while the pointer crosses a seam.
pub(super) fn day_at((x, y): (f32, f32), days: usize) -> Option<usize> {
    if x < GUTTER || y < TOP {
        return None;
    }
    let week = ((x - GUTTER) / PITCH) as usize;
    let row = ((y - TOP) / PITCH) as usize;
    (row < 7)
        .then_some(week * 7 + row)
        .filter(|index| *index < days)
}

pub(super) fn calendar_width(weeks: usize) -> f32 {
    GUTTER + PITCH * weeks.saturating_sub(1) as f32 + CELL
}

/// Column width of the rhythm grid in `width` points.
fn slot_pitch(width: f32) -> f32 {
    ((width - GUTTER) / 24.0).max(4.0)
}

pub(super) fn slot_at((x, y): (f32, f32), width: f32) -> Option<usize> {
    if x < GUTTER || y < TOP {
        return None;
    }
    let hour = ((x - GUTTER) / slot_pitch(width)) as usize;
    let row = ((y - TOP) / PITCH) as usize;
    (row < 7 && hour < 24).then_some(row * 24 + hour)
}

fn date_title(day: i64) -> String {
    let (year, month, date) = activity::civil(day);
    format!(
        "{}, {} {date}, {year}",
        &WEEKDAYS[usize::from(activity::weekday(day))][..3],
        MONTHS[(month - 1) as usize]
    )
}

fn money(value: f64) -> String {
    format!("${value:.2}")
}

fn hours_text(hours: u32) -> String {
    if hours == 1 {
        "1 hour".into()
    } else {
        format!("{hours} hours")
    }
}

fn days_text(days: usize) -> String {
    if days == 1 {
        "1 day".into()
    } else {
        format!("{days} days")
    }
}

fn text(content: impl Into<SharedString>, size: f32, color: Rgba) -> gpui::Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(size))
        .text_color(color)
        .child(content.into())
}

/// A tooltip beside a cell, centered on it, to its right or, flipped, to
/// its left. Deferred so the neighbouring chart never paints over it.
fn anchored((x, y): (f32, f32), flip: bool, card: impl IntoElement) -> impl IntoElement {
    let card = div()
        .flex_none()
        .when(flip, |card| card.mr(px(8.0)))
        .when(!flip, |card| card.ml(px(8.0)))
        .child(card);
    deferred(
        div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(0.0))
            .h(px(0.0))
            .flex()
            .items_center()
            .when(flip, |anchor| anchor.justify_end())
            .child(card),
    )
    .with_priority(1)
}

/// Ely's `ChartTooltip`: a title, then a row per reading with its dot.
fn tooltip(
    title: String,
    rows: Vec<(Option<Rgba>, String, String)>,
    colors: SemanticColors,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .px(px(9.0))
        .py(px(7.0))
        .min_w(px(160.0))
        .rounded(px(8.0))
        // Opaque: cells under a glass fill would read through the numbers.
        .bg(colors.floating_surface().alpha(1.0))
        .border_1()
        .border_color(colors.primary.alpha(0.1))
        .child(text(title, 11.0, colors.secondary).mb(px(1.0)))
        .children(rows.into_iter().map(|(dot, name, value)| {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .children(dot.map(|dot| div().size(px(6.0)).rounded(px(3.0)).bg(dot)))
                .child(div().flex_1().child(text(name, 11.0, colors.secondary)))
                .child(div().pl(px(12.0)).child(if value.contains(' ') {
                    // "5 hours", "14 of 26": words keep their own spacing.
                    text(value, 11.0, colors.primary)
                } else {
                    crate::number_flow::tabular(value, 11.0, colors.primary, FontWeight::NORMAL)
                }))
        }))
}

/// Ely's key: five steps between two words.
fn key(steps: [Rgba; 5], colors: SemanticColors) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(3.0))
        .child(text("Less", 10.0, colors.tertiary).mr(px(2.0)))
        .children(steps.map(|step| div().size(px(CELL - 1.0)).rounded(px(CELL_RADIUS)).bg(step)))
        .child(text("More", 10.0, colors.tertiary).ml(px(2.0)))
}

impl UtilitySurfaces {
    /// The bins for the selected source, rebuilt only when the snapshot,
    /// source, day, or week start changes.
    fn usage_activity_data(&self, now: i64) -> (Rc<Activity>, Rc<[IntensityScale; 3]>) {
        let host = self.usage_host.clone();
        let remote = self.usage.remote_sources(host.as_deref());
        let key = ActivityKey {
            history: Arc::as_ptr(&self.usage.history) as usize,
            updated_at: self.usage.updated_at,
            now,
            host,
            remote: remote
                .iter()
                .map(|data| (*data as *const _ as usize, data.collected_at))
                .collect(),
            week_start: self.usage_week_start,
        };
        if let Some(cache) = self.usage_activity.borrow().as_ref()
            && cache.key == key
        {
            return (Rc::clone(&cache.activity), Rc::clone(&cache.scales));
        }
        let local = key
            .host
            .as_deref()
            .is_none_or(str::is_empty)
            .then_some(&*self.usage.history);
        let built = activity::build(
            local,
            &remote,
            now,
            CALENDAR_WEEKS,
            key.week_start,
            &activity::system_offset,
        );
        let scale = |metric| {
            IntensityScale::new(
                built
                    .days
                    .iter()
                    .map(move |day: &ActivityDay| day.value(metric)),
            )
        };
        let scales = Rc::new([
            scale(ActivityMetric::Hours),
            scale(ActivityMetric::Tokens),
            scale(ActivityMetric::Cost),
        ]);
        let built = Rc::new(built);
        *self.usage_activity.borrow_mut() = Some(ActivityCache {
            key,
            activity: Rc::clone(&built),
            scales: Rc::clone(&scales),
        });
        (built, scales)
    }

    fn set_usage_activity_hover(&mut self, hover: Option<ActivityHover>, cx: &mut Context<Self>) {
        if self.usage_activity_hover != hover {
            self.usage_activity_hover = hover;
            cx.notify();
        }
    }

    pub(super) fn usage_activity(
        &self,
        now: i64,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (activity, scales) = self.usage_activity_data(now);
        let metric = if activity.hourly {
            self.usage_activity_metric
        } else if self.usage_activity_metric == ActivityMetric::Hours {
            ActivityMetric::Tokens
        } else {
            self.usage_activity_metric
        };
        let steps = steps(colors);
        let streaks = activity.streaks();
        let mut summary = vec![format!("{} active", days_text(activity.active_days()))];
        if activity.hourly {
            summary.push(format!(
                "{} with agents at work",
                hours_text(activity.active_hours())
            ));
        }
        summary.push(format!("longest streak {}", days_text(streaks.longest)));
        if streaks.current > 0 {
            summary.push(format!("current {}", days_text(streaks.current)));
        }
        let mut metrics = div().flex().gap(px(3.0));
        for (option, name) in [
            (ActivityMetric::Hours, "Hours"),
            (ActivityMetric::Tokens, "Tokens"),
            (ActivityMetric::Cost, "Cost"),
        ] {
            if option == ActivityMetric::Hours && !activity.hourly {
                continue;
            }
            metrics = metrics.child(
                usage_control(
                    format!("usage-activity-{}", name.to_lowercase()),
                    name,
                    metric == option,
                    colors,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.usage_activity_metric = option;
                    cx.notify();
                })),
            );
        }
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(text("Activity", 13.0, colors.primary).font_weight(FontWeight::MEDIUM))
                    .child(text(summary.join(" · "), 11.0, colors.secondary)),
            )
            .child(metrics);
        let calendar = self.usage_calendar(
            &activity,
            &scales[metric_index(metric)],
            metric,
            steps,
            colors,
            cx,
        );
        let mut charts = div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap_x(px(36.0))
            .gap_y(px(20.0))
            .child(calendar);
        if activity.hourly {
            charts = charts.child(self.usage_rhythm(&activity, colors, cx));
        }
        let remote = self
            .usage_host
            .as_deref()
            .is_none_or(|host| !host.is_empty())
            && !self.usage.remote.is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(header)
            .child(charts)
            .when(remote, |section| {
                section.child(text(
                    if activity.hourly {
                        "Remote machines report daily totals: they add tokens and cost on their UTC date, not active hours."
                    } else {
                        "Remote machines report daily totals on their UTC date, so hours and the weekly rhythm are available for this Mac only."
                    },
                    11.0,
                    colors.tertiary,
                ).whitespace_normal())
            })
    }

    fn usage_calendar(
        &self,
        activity: &Rc<Activity>,
        scale: &IntensityScale,
        metric: ActivityMetric,
        steps: [Rgba; 5],
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let weeks = activity.weeks();
        let width = calendar_width(weeks);
        let hover = match self.usage_activity_hover {
            Some(ActivityHover::Day(index)) if index < activity.days.len() => Some(index),
            _ => None,
        };
        // Days the ledger never kept are left unpainted, not shown as idle.
        let cells: Vec<(f32, f32, Rgba)> = activity
            .days
            .iter()
            .enumerate()
            .filter(|(index, _)| activity.retained(*index))
            .map(|(index, day)| {
                let (x, y) = day_origin(index);
                (x, y, steps[usize::from(scale.level(day.value(metric)))])
            })
            .collect();
        let today = day_origin(activity.days.len() - 1);
        let hovered = hover.map(day_origin);
        let edge = colors.primary.alpha(0.85);
        let today_edge = colors.primary.alpha(0.3);
        let months = activity.month_columns().into_iter().map(|(week, month)| {
            div()
                .absolute()
                .left(px(GUTTER + PITCH * week as f32))
                .top(px(0.0))
                .child(text(MONTHS[(month - 1) as usize], 10.0, colors.tertiary))
        });
        let weekdays = (0..7)
            .filter(|row| {
                let weekday = (row + usize::from(activity.week_start)) % 7;
                matches!(weekday, 0 | 2 | 4)
            })
            .map(|row| {
                let weekday = (row + usize::from(activity.week_start)) % 7;
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(TOP + PITCH * row as f32 + CELL * 0.5))
                    .w(px(GUTTER - 6.0))
                    .h(px(0.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(text(&WEEKDAYS[weekday][..3], 10.0, colors.tertiary))
            });
        let tip = hover.map(|index| {
            let day = activity.days[index];
            let mut rows = Vec::new();
            if activity.hourly {
                rows.push((None, "Active".to_owned(), hours_text(u32::from(day.hours))));
            }
            rows.push((
                None,
                "Tokens".to_owned(),
                UsageFormat::tokens(day.total_tokens()),
            ));
            rows.push((None, "Cost".to_owned(), money(day.total_cost())));
            for (provider, name) in PROVIDERS.iter().enumerate() {
                let (tokens, cost) = (day.tokens[provider], day.cost[provider]);
                if tokens > 0 || cost > 0.0 {
                    rows.push((
                        Some(super::usage_page::provider_color(provider, colors)),
                        (*name).to_owned(),
                        match metric {
                            ActivityMetric::Cost => money(cost),
                            _ => UsageFormat::tokens(tokens),
                        },
                    ));
                }
            }
            let title = if activity.retained(index) {
                date_title(day.day)
            } else {
                format!("{} · not kept", date_title(day.day))
            };
            let (x, y) = day_origin(index);
            anchored(
                (if x > width * 0.6 { x } else { x + CELL }, y + CELL * 0.5),
                x > width * 0.6,
                tooltip(title, rows, colors),
            )
        });
        let bounds_slot = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let days = activity.days.len();
        let chart = div()
            .id("usage-activity-calendar")
            .relative()
            .w(px(width))
            .h(px(GRID_H))
            .on_mouse_move(cx.listener({
                let bounds_slot = Rc::clone(&bounds_slot);
                move |this, event: &MouseMoveEvent, _, cx| {
                    let Some(bounds) = bounds_slot.get() else {
                        return;
                    };
                    let offset = event.position - bounds.origin;
                    let hover = day_at((f32::from(offset.x), f32::from(offset.y)), days)
                        .map(ActivityHover::Day);
                    this.set_usage_activity_hover(hover, cx);
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && matches!(this.usage_activity_hover, Some(ActivityHover::Day(_))) {
                    this.set_usage_activity_hover(None, cx);
                }
            }))
            .child(
                canvas(
                    {
                        let bounds_slot = Rc::clone(&bounds_slot);
                        move |bounds, _, _| bounds_slot.set(Some(bounds))
                    },
                    move |bounds, _, window, _| {
                        let origin = bounds.origin;
                        let quad = |x: f32, y: f32| {
                            Bounds::new(origin + point(px(x), px(y)), size(px(CELL), px(CELL)))
                        };
                        for (x, y, color) in &cells {
                            window.paint_quad(
                                fill(quad(*x, *y), *color).corner_radii(px(CELL_RADIUS)),
                            );
                        }
                        for (at, color) in [(Some(today), today_edge), (hovered, edge)] {
                            if let Some((x, y)) = at {
                                window.paint_quad(
                                    gpui::outline(quad(x, y), color, gpui::BorderStyle::Solid)
                                        .corner_radii(px(CELL_RADIUS)),
                                );
                            }
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .children(months)
            .children(weekdays)
            .children(tip);
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .flex_none()
            .w(px(width))
            .child(chart)
            .child(
                div()
                    .pl(px(GUTTER))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(text(
                        match metric {
                            ActivityMetric::Hours => "Active hours a day",
                            ActivityMetric::Tokens => "Tokens a day",
                            ActivityMetric::Cost => "Estimated cost a day",
                        },
                        10.0,
                        colors.tertiary,
                    ))
                    .child(key(steps, colors)),
            )
    }

    fn usage_rhythm(
        &self,
        activity: &Rc<Activity>,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = Ink::on_surface(Ink::FRESH, colors);
        let empty = colors.primary.alpha(0.06);
        let highest = activity
            .rhythm
            .iter()
            .flatten()
            .map(|cell| cell.hours)
            .max()
            .unwrap_or(0);
        let strength = move |hours: u32| {
            if hours == 0 || highest == 0 {
                empty
            } else {
                ink.alpha(0.12 + 0.88 * hours as f32 / highest as f32)
            }
        };
        let colors_by_slot: Vec<Rgba> = activity
            .rhythm
            .iter()
            .flatten()
            .map(|cell| strength(cell.hours))
            .collect();
        let hover = match self.usage_activity_hover {
            Some(ActivityHover::Slot(slot)) if slot < 7 * 24 => Some(slot),
            _ => None,
        };
        let bounds_slot = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let width = bounds_slot_width(&self.usage_rhythm_width);
        let pitch = slot_pitch(width);
        let weekdays = (0..7).map(|row| {
            let weekday = (row + usize::from(activity.week_start)) % 7;
            div()
                .absolute()
                .left(px(0.0))
                .top(px(TOP + PITCH * row as f32 + CELL * 0.5))
                .w(px(GUTTER - 6.0))
                .h(px(0.0))
                .flex()
                .items_center()
                .justify_end()
                .child(text(&WEEKDAYS[weekday][..3], 10.0, colors.tertiary))
        });
        let hours = [0, 6, 12, 18].map(|hour| {
            div()
                .absolute()
                .left(px(GUTTER + pitch * hour as f32))
                .top(px(0.0))
                .child(text(format!("{hour:02}:00"), 10.0, colors.tertiary))
        });
        let tip = hover.map(|slot| {
            let (row, hour) = (slot / 24, slot % 24);
            let weekday = WEEKDAYS[(row + usize::from(activity.week_start)) % 7];
            let cell = activity.rhythm[row][hour];
            let x = GUTTER + pitch * hour as f32;
            let y = TOP + PITCH * row as f32;
            let flip = x > width * 0.6;
            anchored(
                (if flip { x } else { x + pitch - 3.0 }, y + CELL * 0.5),
                flip,
                tooltip(
                    format!("{weekday}s · {hour:02}:00–{:02}:00", (hour + 1) % 24),
                    vec![
                        (
                            Some(strength(cell.hours)),
                            "Active".to_owned(),
                            format!("{} of {}", cell.hours, activity.rhythm_days[row]),
                        ),
                        (None, "Tokens".to_owned(), UsageFormat::tokens(cell.tokens)),
                    ],
                    colors,
                ),
            )
        });
        let hovered = hover;
        let edge = colors.primary.alpha(0.85);
        let width_cell = Rc::clone(&self.usage_rhythm_width);
        let chart = div()
            .id("usage-activity-rhythm")
            .relative()
            .w_full()
            .h(px(GRID_H))
            .on_mouse_move(cx.listener({
                let bounds_slot = Rc::clone(&bounds_slot);
                move |this, event: &MouseMoveEvent, _, cx| {
                    let Some(bounds) = bounds_slot.get() else {
                        return;
                    };
                    let offset = event.position - bounds.origin;
                    let hover = slot_at(
                        (f32::from(offset.x), f32::from(offset.y)),
                        f32::from(bounds.size.width),
                    )
                    .map(ActivityHover::Slot);
                    this.set_usage_activity_hover(hover, cx);
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && matches!(this.usage_activity_hover, Some(ActivityHover::Slot(_))) {
                    this.set_usage_activity_hover(None, cx);
                }
            }))
            .child(
                canvas(
                    {
                        let bounds_slot = Rc::clone(&bounds_slot);
                        move |bounds, _, _| {
                            bounds_slot.set(Some(bounds));
                            width_cell.set(f32::from(bounds.size.width));
                        }
                    },
                    move |bounds, _, window, _| {
                        let pitch = slot_pitch(f32::from(bounds.size.width));
                        let cell_w = (pitch - 3.0).max(1.0);
                        let quad = |slot: usize| {
                            let (row, hour) = (slot / 24, slot % 24);
                            Bounds::new(
                                bounds.origin
                                    + point(
                                        px(GUTTER + pitch * hour as f32),
                                        px(TOP + PITCH * row as f32),
                                    ),
                                size(px(cell_w), px(CELL)),
                            )
                        };
                        for (slot, color) in colors_by_slot.iter().enumerate() {
                            window
                                .paint_quad(fill(quad(slot), *color).corner_radii(px(CELL_RADIUS)));
                        }
                        if let Some(slot) = hovered {
                            window.paint_quad(
                                gpui::outline(quad(slot), edge, gpui::BorderStyle::Solid)
                                    .corner_radii(px(CELL_RADIUS)),
                            );
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .children(hours)
            .children(weekdays)
            .children(tip);
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .flex_1()
            .min_w(px(300.0))
            .child(chart)
            .child(div().pl(px(GUTTER)).child(text(
                "When you work · active hours by weekday and local hour",
                10.0,
                colors.tertiary,
            )))
    }
}

/// The rhythm chart's last measured width, so the hour labels laid out this
/// frame line up with the cells painted from the measured bounds.
fn bounds_slot_width(width: &Rc<Cell<f32>>) -> f32 {
    let width = width.get();
    if width > 0.0 { width } else { 360.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_hit_testing_matches_the_painted_cells() {
        let days = 7 * 26 + 3;
        for index in [0, 1, 6, 7, 50, days - 1] {
            let (x, y) = day_origin(index);
            assert_eq!(day_at((x + 1.0, y + 1.0), days), Some(index));
            assert_eq!(day_at((x + CELL - 0.5, y + CELL - 0.5), days), Some(index));
        }
        // Gutters and the not-yet days of this week are not cells.
        assert_eq!(day_at((GUTTER - 1.0, TOP + 1.0), days), None);
        assert_eq!(day_at((GUTTER + 1.0, TOP - 1.0), days), None);
        let (x, y) = day_origin(days - 1);
        assert_eq!(day_at((x + 1.0, y + PITCH + 1.0), days), None);
        assert_eq!(day_at((GUTTER + 1.0, TOP + PITCH * 7.0 + 1.0), days), None);
        assert!((calendar_width(27) - (GUTTER + 26.0 * PITCH + CELL)).abs() < 0.01);
    }

    #[test]
    fn rhythm_hit_testing_spans_the_width() {
        let width = GUTTER + 24.0 * 15.0;
        assert_eq!(slot_at((GUTTER + 1.0, TOP + 1.0), width), Some(0));
        assert_eq!(
            slot_at((GUTTER + 15.0 * 23.0 + 1.0, TOP + PITCH * 6.0 + 1.0), width),
            Some(6 * 24 + 23)
        );
        assert_eq!(slot_at((width + 20.0, TOP + 1.0), width), None);
        assert_eq!(slot_at((GUTTER + 1.0, 2.0), width), None);
    }

    #[test]
    fn titles_read_as_calendar_dates() {
        let day = activity::civil_day(2028, 2, 29);
        assert_eq!(date_title(day), "Tue, Feb 29, 2028");
        assert_eq!(hours_text(1), "1 hour");
        assert_eq!(days_text(3), "3 days");
    }
}
