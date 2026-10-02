//! A looping SVG animation that costs nothing to keep running.
//!
//! Every frame is rasterized into the sprite atlas once. The vendored GPUI
//! swaps the visible frame's tile in the already-drawn scene on each step and
//! presents it again (`Window::paint_animated_svg`), so a spinning mark never
//! notifies, renders, lays out or paints its view, or any ancestor of it.

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Rgba, SharedString, Style, TransformationMatrix, Window, px, size,
};
use std::time::Duration;

/// `frames` (asset paths), one every `interval`, at `size` points square.
pub struct FrameLoop {
    frames: &'static [&'static str],
    interval: Duration,
    size: f32,
    color: Rgba,
}

impl FrameLoop {
    pub fn new(
        frames: &'static [&'static str],
        interval: Duration,
        size: f32,
        color: Rgba,
    ) -> Self {
        Self {
            frames,
            interval,
            size,
            color,
        }
    }
}

impl IntoElement for FrameLoop {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FrameLoop {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: size(px(self.size).into(), px(self.size).into()),
            flex_shrink: 0.0,
            ..Style::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(&first) = self.frames.first() else {
            return;
        };
        if cx.reduce_motion() {
            let _ = window.paint_svg(
                bounds,
                first.into(),
                None,
                TransformationMatrix::unit(),
                self.color.into(),
                cx,
            );
            return;
        }
        let frames: Vec<SharedString> = self.frames.iter().map(|&path| path.into()).collect();
        let _ = window.paint_animated_svg(bounds, &frames, self.interval, self.color.into(), cx);
    }
}
