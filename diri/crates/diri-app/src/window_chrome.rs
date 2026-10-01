//! The window caption on platforms that do not draw it into diri's toolbar.
//!
//! On macOS the traffic lights float over the top-left corner of diri's own
//! toolbar. Windows has no such hybrid: its native caption is a separate
//! strip above the app. So on Windows the window opens with a transparent
//! titlebar, which makes GPUI hide that strip and keep only the resize frame.
//! Diri then draws minimize, maximize and close into the top-right corner of
//! whichever toolbar reaches it, the way the traffic lights sit in the
//! top-left one, and every toolbar in the title row marks itself as the
//! caption's drag area.
//!
//! Both are `WindowControlArea`s, so Windows still drives them: dragging and
//! double-clicking the toolbar, Aero Snap, the system menu on right-click and
//! the Snap Layouts flyout on hovering maximize all behave natively. A control
//! area only claims the pointer where its own hitbox is the frontmost one
//! (`vendor/gpui/DIRI_PATCHES.md`), so toolbar buttons drawn over a drag area
//! keep working without occluding it.

use diri_ui::{Fill, IconName, Metrics, SemanticColors};
use gpui::{AnyElement, InteractiveElement, Rgba, Window, WindowControlArea, div, prelude::*, px, svg};

/// Windows 11's caption buttons are 46 px wide whatever the title height.
pub(crate) const CAPTION_BUTTON_WIDTH: f32 = 46.0;

/// The close button's hover fill on Windows 11, in both light and dark mode.
const CLOSE_HOVER: Rgba = Rgba {
    r: 196.0 / 255.0,
    g: 43.0 / 255.0,
    b: 28.0 / 255.0,
    a: 1.0,
};

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Lays the window out as Windows does from any platform, so layout tests and
/// screenshot fixtures can exercise the caption on a Mac.
#[cfg(test)]
pub(crate) fn force_caption_buttons(forced: bool) {
    FORCED.with(|cell| cell.set(forced));
}

/// Whether diri draws the window's caption buttons itself.
pub(crate) fn draws_caption_buttons() -> bool {
    #[cfg(test)]
    if FORCED.with(std::cell::Cell::get) {
        return true;
    }
    cfg!(windows)
}

/// Width that the toolbar reaching the window's top-right corner leaves free
/// for the caption buttons.
pub(crate) fn caption_lane() -> f32 {
    if draws_caption_buttons() {
        3.0 * CAPTION_BUTTON_WIDTH
    } else {
        0.0
    }
}

/// Width that the toolbar reaching the window's top-left corner leaves free
/// for macOS's traffic lights.
pub(crate) fn traffic_light_lane() -> f32 {
    if cfg!(target_os = "macos") && !draws_caption_buttons() {
        Metrics::TOOLBAR_TRAFFIC_LIGHT_LANE
    } else {
        0.0
    }
}

/// How much of the caption lane a toolbar must leave free at its trailing
/// edge, given where it starts and how far its right edge stops short of the
/// window's. It grows continuously as a neighbour such as the inspector slides
/// away, so trailing actions never jump.
pub(crate) fn caption_inset(top: f32, right_gap: f32) -> f32 {
    if top > 0.5 {
        0.0
    } else {
        (caption_lane() - right_gap.max(0.0)).max(0.0)
    }
}

pub(crate) trait TitlebarDragArea: InteractiveElement + Sized {
    /// Marks this title-row toolbar as somewhere the window can be dragged
    /// from. Its buttons stay buttons.
    fn titlebar_drag_area(self) -> Self {
        if draws_caption_buttons() {
            self.window_control_area(WindowControlArea::Drag)
        } else {
            self
        }
    }
}

impl<E: InteractiveElement> TitlebarDragArea for E {}

/// Minimize, maximize or restore, and close, laid out for the top-right
/// corner of the window and as tall as the title row.
pub(crate) fn caption_buttons(window: &Window, colors: SemanticColors) -> Option<AnyElement> {
    if !draws_caption_buttons() || window.is_fullscreen() {
        return None;
    }
    // Windows dims an inactive window's caption glyphs; the toolbar around
    // them stays as it is.
    let ink = if window.is_window_active() {
        colors.primary
    } else {
        colors.tertiary
    };
    let maximized = window.is_maximized();
    let button = |id: &'static str, label: &'static str, icon: IconName, area: WindowControlArea| {
        let close = area == WindowControlArea::Close;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .w(px(CAPTION_BUTTON_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .group(id)
            .window_control_area(area)
            .map(|button| {
                if close {
                    button
                        .hover(|button| button.bg(CLOSE_HOVER))
                        .active(|button| button.bg(CLOSE_HOVER.alpha(0.9)))
                } else {
                    button
                        .hover(move |button| button.bg(Fill::subtle(colors)))
                        .active(move |button| button.bg(colors.primary.alpha(0.04)))
                }
            })
            .child(
                svg()
                    .path(icon.asset_path())
                    .flex_none()
                    .size(px(10.0))
                    .text_color(ink)
                    // White over the red fill, as Windows draws it. Group
                    // hover gives the glyph a hitbox of its own in front of
                    // the button's, so it reports the button's area too.
                    .when(close, |glyph| {
                        glyph
                            .group_hover(id, |glyph| glyph.text_color(gpui::white()))
                            .window_control_area(area)
                    }),
            )
    };
    Some(
        div()
            .id("window-caption-buttons")
            .debug_selector(|| "window-caption-buttons".into())
            .absolute()
            .top_0()
            .right_0()
            .h(px(Metrics::TITLE_BAR))
            .flex()
            .child(button(
                "window-minimize",
                "Minimize",
                IconName::WindowMinimize,
                WindowControlArea::Min,
            ))
            .child(if maximized {
                button(
                    "window-restore",
                    "Restore",
                    IconName::WindowRestore,
                    WindowControlArea::Max,
                )
            } else {
                button(
                    "window-maximize",
                    "Maximize",
                    IconName::WindowMaximize,
                    WindowControlArea::Max,
                )
            })
            .child(button(
                "window-close",
                "Close",
                IconName::WindowClose,
                WindowControlArea::Close,
            ))
            .into_any_element(),
    )
}
