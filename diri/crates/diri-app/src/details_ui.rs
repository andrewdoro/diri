// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! Small display primitives for the inspector's Details surface: section
//! headers, description lists, grouped list rows, tags, avatars, and the
//! dotted diff stat. Ported from Ely's `data_display`, `lists`, and
//! `git/badges` and restated in diri's tokens.
//!
//! Every fill is a translucent tint of the foreground, never a fixed surface,
//! so the same pieces read on the glass sidebar and on a terminal-colored
//! panel, light or dark.

use diri_ui::{Icon, IconName, Ink, Radius, SemanticColors, Typo};
use gpui::{
    AnyElement, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Stateful, Styled, div, prelude::*, px, rgba,
};

/// Vertical rhythm between the panel's sections.
pub const SECTION_GAP: f32 = 18.0;
/// Horizontal inset of the scrolling content.
pub const CONTENT_INSET: f32 = 12.0;
/// Width of the label column in a description list.
const LABEL_WIDTH: f32 = 74.0;

/// Selected quote tint, shared by every selectable block on the surface.
pub fn selection_fill() -> gpui::Rgba {
    rgba(0x5b8fd12f)
}

pub fn selection_stroke() -> gpui::Rgba {
    rgba(0x8bb9e8aa)
}

pub fn selection_hover() -> gpui::Rgba {
    rgba(0x5b8fd11a)
}

/// GitHub's merged purple, which no semantic ink carries.
pub const MERGED: gpui::Rgba = gpui::Rgba {
    r: 0.686,
    g: 0.486,
    b: 0.969,
    a: 1.0,
};

/// A tintable icon at an exact size (the legacy symbol bridge snaps to 14pt
/// and up, too large for inline marks).
pub fn icon(name: IconName, size: f32, color: gpui::Rgba) -> AnyElement {
    Icon::new(name, size, color).into_any_element()
}

/// A section: a quiet title with an optional count or trailing element, then
/// its body. `flex_none` keeps a tall body from being squeezed by the
/// scrolling column it sits in.
pub fn section(
    title: &'static str,
    trailing: Option<AnyElement>,
    body: impl IntoElement,
    colors: SemanticColors,
) -> Div {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(7.0))
        .child(section_header(title, trailing, colors))
        .child(body)
}

pub fn section_header(
    title: &'static str,
    trailing: Option<AnyElement>,
    colors: SemanticColors,
) -> AnyElement {
    div()
        .px(px(2.0))
        .h(px(16.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(Typo::SECTION_HEADER.size))
        .child(
            div()
                .font_weight(Typo::SECTION_HEADER.weight)
                .text_color(colors.secondary)
                .child(title),
        )
        .when_some(trailing, |header, trailing| {
            header.child(
                div()
                    .min_w(px(0.0))
                    .ml_auto()
                    .flex()
                    .items_center()
                    .text_color(colors.tertiary)
                    .child(trailing),
            )
        })
        .into_any_element()
}

/// A count beside a section title, in the quiet tone.
pub fn count_label(count: usize, colors: SemanticColors) -> AnyElement {
    div()
        .text_size(px(Typo::META.size))
        .font_weight(FontWeight::NORMAL)
        .text_color(colors.tertiary)
        .child(count.to_string())
        .into_any_element()
}

/// The one grouping surface: a faint tint and hairline, used only where rows
/// belong together (a list of links, a pull request).
pub fn card(colors: SemanticColors) -> Div {
    div()
        .flex_none()
        .rounded(px(Radius::CARD))
        .bg(colors.primary.alpha(0.028))
        .border_1()
        .border_color(colors.primary.alpha(0.065))
        .overflow_hidden()
}

/// A hairline between rows of a grouped list.
pub fn hairline(colors: SemanticColors) -> Div {
    div().h(px(1.0)).flex_none().bg(colors.primary.alpha(0.055))
}

/// Labels and values, one pair a row with the labels in a quiet column
/// (Ely's `DescriptionList`, lined).
pub struct DescriptionList {
    rows: Vec<(&'static str, AnyElement)>,
}

impl DescriptionList {
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    pub fn item(mut self, label: &'static str, value: impl IntoElement) -> Self {
        self.rows.push((label, value.into_any_element()));
        self
    }

    /// A truncating text value, optionally monospaced (paths, branches).
    pub fn text(
        self,
        label: &'static str,
        value: impl Into<String>,
        monospaced: bool,
        colors: SemanticColors,
    ) -> Self {
        let value: String = value.into();
        self.item(
            label,
            div()
                .min_w(px(0.0))
                .truncate()
                .when(monospaced, |value| {
                    value.font_family(crate::fonts::mono_family())
                })
                .text_size(px(if monospaced { 11.0 } else { 12.0 }))
                .text_color(colors.primary.alpha(0.82))
                .child(value),
        )
    }

    pub fn render(self, colors: SemanticColors) -> AnyElement {
        let last = self.rows.len().saturating_sub(1);
        div()
            .flex_none()
            .flex()
            .flex_col()
            .children(
                self.rows
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, value))| {
                        div()
                            .min_h(px(28.0))
                            .px(px(2.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .when(index < last, |row| {
                                row.border_b_1().border_color(colors.primary.alpha(0.05))
                            })
                            .child(
                                div()
                                    .w(px(LABEL_WIDTH))
                                    .flex_none()
                                    .text_size(px(Typo::META.size))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(colors.tertiary)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .min_w(px(0.0))
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .child(value),
                            )
                    }),
            )
            .into_any_element()
    }
}

impl Default for DescriptionList {
    fn default() -> Self {
        Self::new()
    }
}

/// A short tinted label: a state, a verdict, a count.
pub fn tag(label: impl Into<String>, color: gpui::Rgba) -> AnyElement {
    let label: String = label.into();
    div()
        .flex_none()
        .h(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(Radius::CHIP))
        .bg(color.alpha(0.13))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(label)
        .into_any_element()
}

/// A tag with a leading icon (pull request state).
pub fn icon_tag(name: IconName, label: &'static str, color: gpui::Rgba) -> AnyElement {
    div()
        .flex_none()
        .h(px(20.0))
        .pl(px(5.0))
        .pr(px(7.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded_full()
        .bg(color.alpha(0.14))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(icon(name, 11.0, color))
        .child(label)
        .into_any_element()
}

/// A neutral monospaced ref label that truncates rather than overflowing.
pub fn ref_tag(name: impl Into<String>, colors: SemanticColors) -> AnyElement {
    let name: String = name.into();
    div()
        .min_w(px(0.0))
        .flex_shrink(1.0)
        .h(px(19.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(Radius::CHIP))
        .bg(colors.primary.alpha(0.06))
        .font_family(crate::fonts::mono_family())
        .text_size(px(10.5))
        .text_color(colors.secondary)
        .child(div().min_w(px(0.0)).truncate().child(name))
        .into_any_element()
}

/// Up to two capitals from a name's first and last words; a handle like
/// `yyx990803` gives its first letter.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|character: char| character.is_whitespace() || character == '-' || character == '_')
        .filter(|word| !word.is_empty())
        .collect();
    let first = |word: &str| {
        word.chars()
            .find(|character| character.is_alphanumeric())
            .into_iter()
            .flat_map(char::to_uppercase)
    };
    let initials: String = match words.as_slice() {
        [] => String::new(),
        [only] => first(only).collect(),
        [head, .., tail] => first(head).chain(first(tail)).collect(),
    };
    if initials.is_empty() {
        "?".to_owned()
    } else {
        initials
    }
}

const AVATAR_HUES: [gpui::Rgba; 8] = [
    gpui::Rgba {
        r: 0.36,
        g: 0.56,
        b: 0.96,
        a: 1.0,
    },
    gpui::Rgba {
        r: 0.20,
        g: 0.70,
        b: 0.55,
        a: 1.0,
    },
    gpui::Rgba {
        r: 0.86,
        g: 0.47,
        b: 0.34,
        a: 1.0,
    },
    gpui::Rgba {
        r: 0.69,
        g: 0.49,
        b: 0.97,
        a: 1.0,
    },
    gpui::Rgba {
        r: 0.90,
        g: 0.62,
        b: 0.18,
        a: 1.0,
    },
    gpui::Rgba {
        r: 0.88,
        g: 0.38,
        b: 0.58,
        a: 1.0,
    },
    gpui::Rgba {
        r: 0.25,
        g: 0.66,
        b: 0.80,
        a: 1.0,
    },
    gpui::Rgba {
        r: 0.56,
        g: 0.62,
        b: 0.30,
        a: 1.0,
    },
];

/// The hue a name keeps, so the same person reads the same way everywhere.
pub fn avatar_hue_index(name: &str) -> usize {
    let hash = name.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    hash as usize % AVATAR_HUES.len()
}

/// Initials on a tone kept by the name.
pub fn avatar(name: &str, size: f32) -> AnyElement {
    let hue = AVATAR_HUES[avatar_hue_index(name)];
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(hue.alpha(0.20))
        .text_size(px((size * 0.42).max(8.5)))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hue)
        .child(initials(name))
        .into_any_element()
}

/// Overlapping avatars, each ringed by a gap so they stay apart.
pub fn avatar_group<'a>(names: impl IntoIterator<Item = &'a str>, size: f32) -> AnyElement {
    div()
        .flex()
        .items_center()
        .children(names.into_iter().enumerate().map(|(index, name)| {
            div()
                .when(index > 0, |avatar| avatar.ml(px(-size * 0.28)))
                .rounded_full()
                .child(avatar(name, size))
        }))
        .into_any_element()
}

/// Dots a diff stat shows.
const SQUARES: usize = 5;

/// How many of the dots read as added, the rest as removed, when anything
/// changed.
pub fn diff_split(added: u64, removed: u64) -> Option<usize> {
    let total = added + removed;
    (total > 0).then(|| ((added * SQUARES as u64 + total / 2) / total) as usize)
}

/// Lines added and removed: the two counts, and five dots shared between them.
pub fn diff_stat(added: u64, removed: u64, colors: SemanticColors) -> AnyElement {
    let green = diff_split(added, removed);
    let square = |index: usize| {
        let color = match green {
            Some(green) if index < green => Ink::FRESH,
            Some(_) => Ink::DANGER,
            None => colors.primary.alpha(0.12),
        };
        div().size(px(7.0)).rounded(px(1.5)).bg(color)
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(Typo::META.size))
        .font_weight(FontWeight::SEMIBOLD)
        .child(div().text_color(Ink::FRESH).child(format!("+{added}")))
        .child(div().text_color(Ink::DANGER).child(format!("−{removed}")))
        .child(div().flex().gap(px(2.0)).children((0..SQUARES).map(square)))
        .into_any_element()
}

/// A square icon tile that leads a list row.
pub fn icon_tile(name: IconName, tint: gpui::Rgba, colors: SemanticColors) -> AnyElement {
    div()
        .size(px(26.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(Radius::BADGE))
        .bg(colors.primary.alpha(0.06))
        .child(icon(name, 14.0, tint))
        .into_any_element()
}

/// One row of a grouped list (Ely's `ListItem`): a leading element, a title
/// over an optional subtitle, and a trailing element. The caller attaches
/// the click.
pub fn list_row(
    id: impl Into<ElementId>,
    leading: AnyElement,
    title: impl Into<String>,
    subtitle: Option<String>,
    trailing: Option<AnyElement>,
    colors: SemanticColors,
) -> Stateful<Div> {
    let title: String = title.into();
    div()
        .id(id)
        .min_h(px(44.0))
        .px(px(9.0))
        .py(px(7.0))
        .flex()
        .items_center()
        .gap(px(9.0))
        .cursor_pointer()
        .hover(move |row| row.bg(colors.primary.alpha(0.045)))
        .child(leading)
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .truncate()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.primary)
                        .child(title),
                )
                .when_some(subtitle, |text, subtitle| {
                    text.child(
                        div()
                            .truncate()
                            .text_size(px(Typo::META.size))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(colors.tertiary)
                            .child(subtitle),
                    )
                }),
        )
        .when_some(trailing, |row, trailing| {
            row.child(div().flex_none().child(trailing))
        })
}

/// A compact ghost button: icon and optional label, filled with `hover`
/// under the pointer.
pub fn ghost_button(
    id: impl Into<ElementId>,
    name: IconName,
    label: Option<&'static str>,
    tint: gpui::Rgba,
    hover: gpui::Rgba,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(22.0))
        .when(label.is_none(), |button| {
            button.w(px(22.0)).justify_center()
        })
        .when(label.is_some(), |button| button.px(px(7.0)))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded(px(Radius::CHIP))
        .cursor_pointer()
        .hover(move |button| button.bg(hover))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tint)
        .child(icon(name, 12.0, tint))
        .when_some(label, |button, label| button.child(label))
}

/// One segment of a compact segmented control. The track is
/// [`segmented_track`].
pub fn segment(
    id: impl Into<ElementId>,
    label: &'static str,
    count: Option<usize>,
    active: bool,
    colors: SemanticColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .min_w(px(0.0))
        .flex_1()
        .h(px(22.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(5.0))
        .rounded(px(Radius::CHIP))
        .cursor_pointer()
        .when(active, |segment| {
            segment
                .bg(segment_fill(colors))
                .border_1()
                .border_color(colors.primary.alpha(0.07))
        })
        .when(!active, |segment| {
            segment.hover(move |segment| segment.bg(colors.primary.alpha(0.05)))
        })
        .text_size(px(11.5))
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .text_color(if active {
            colors.primary
        } else {
            colors.secondary
        })
        .child(label)
        .when_some(count, |segment, count| {
            segment.child(
                div()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.tertiary)
                    .child(count.to_string()),
            )
        })
}

pub fn segmented_track(colors: SemanticColors) -> Div {
    div()
        .min_w(px(0.0))
        .flex_1()
        .h(px(26.0))
        .p(px(2.0))
        .flex()
        .items_center()
        .gap(px(2.0))
        .rounded(px(Radius::BADGE))
        .bg(colors.primary.alpha(0.055))
}

/// The raised segment: lighter than the track in either appearance.
fn segment_fill(colors: SemanticColors) -> gpui::Rgba {
    match colors.appearance {
        diri_ui::Appearance::Light => gpui::Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 0.92,
        },
        diri_ui::Appearance::Dark => colors.primary.alpha(0.12),
    }
}

/// How long ago a millisecond timestamp was: `now`, `5m ago`, `3h ago`.
pub fn relative_time(milliseconds: f64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
    relative_time_at(now, milliseconds)
}

pub fn relative_time_at(now: f64, milliseconds: f64) -> String {
    let seconds = ((now - milliseconds).max(0.0) / 1000.0) as u64;
    match seconds {
        0..=59 => "now".to_owned(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        86_400..=2_591_999 => format!("{}d ago", seconds / 86_400),
        2_592_000..=31_535_999 => format!("{}mo ago", seconds / 2_592_000),
        _ => format!("{}y ago", seconds / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_steps_through_units() {
        let now = 1_000_000_000_000.0;
        assert_eq!(relative_time_at(now, now + 5_000.0), "now", "future clamps");
        assert_eq!(relative_time_at(now, now - 59_000.0), "now");
        assert_eq!(relative_time_at(now, now - 5.0 * 60_000.0), "5m ago");
        assert_eq!(relative_time_at(now, now - 3.0 * 3_600_000.0), "3h ago");
        assert_eq!(relative_time_at(now, now - 4.0 * 86_400_000.0), "4d ago");
        assert_eq!(relative_time_at(now, now - 75.0 * 86_400_000.0), "2mo ago");
        assert_eq!(relative_time_at(now, now - 800.0 * 86_400_000.0), "2y ago");
    }

    #[test]
    fn diff_dots_split_by_share() {
        assert_eq!(diff_split(0, 0), None);
        assert_eq!(diff_split(10, 0), Some(5));
        assert_eq!(diff_split(120, 45), Some(4));
        assert_eq!(diff_split(1, 9), Some(1), "a little still shows");
        assert_eq!(diff_split(0, 7), Some(0));
    }

    #[test]
    fn initials_take_first_and_last_words() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("grace brewster murray hopper"), "GH");
        assert_eq!(initials("yyx990803"), "Y");
        assert_eq!(initials("dependabot[bot]"), "D");
        assert_eq!(initials("some-user_name"), "SN");
        assert_eq!(initials(""), "?");
        assert_eq!(initials("---"), "?");
    }

    #[test]
    fn avatar_hue_is_stable_per_name() {
        assert_eq!(avatar_hue_index("antfu"), avatar_hue_index("antfu"));
        assert!(avatar_hue_index("antfu") < AVATAR_HUES.len());
    }
}
