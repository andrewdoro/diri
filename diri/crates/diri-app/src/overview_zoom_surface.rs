//! Painting and input for the Safari-style zoom between the workbench page
//! and its card in the calm Session Overview. The state machine lives in
//! `crate::overview_zoom`; this file only maps it onto the store and onto
//! elements.
//!
//! The calm overview's cards are the pane itself at a smaller font in the
//! pane's own aspect, so nothing is swapped mid-flight: the flying element is
//! the session's terminal drawn through the same card builder the grid uses,
//! in an interpolated frame at an interpolated (ladder-snapped) font size,
//! with the card's title strip and chrome fading in. At progress 1 it is the
//! card, pixel for pixel.
use super::overview_calm::miniature_box;
use super::*;
use crate::overview_zoom::{
    CardPose, Grip, Landing, OverviewZoom, ZoomRect, card_pose, grid_alpha, page_frame,
};
use crate::terminal_pane::TerminalViewport;
use diri_term::metrics::CellMetrics;
use std::cell::RefCell;
use std::rc::Rc;

/// The pane's grid inset below its title bar and inside its left border:
/// `render_grid_and_overlays` pads the grid 12 pt sideways and 2 pt on top.
const PAGE_GRID_LEFT: f32 = 12.0;
const PAGE_GRID_TOP: f32 = 1.0;
/// The pane's grid bottom padding, for a grid that overflows and anchors low.
const PAGE_GRID_MIN_BOTTOM: f32 = 9.0;

/// Everything the zoom needs besides the pure state machine.
#[derive(Default)]
pub(super) struct ZoomPresentation {
    pub(super) zoom: OverviewZoom,
    /// The session playing the page: the one you pinched away from, or the
    /// card you pinched (or clicked) open.
    pub(super) session: Option<SessionId>,
    pub(super) grip: Option<Grip>,
    /// The primary pane in window coordinates, supplied by RootView.
    pub(super) page: ZoomRect,
    /// The pane's title bar height (0 under the horizontal tab strip).
    pub(super) page_header: f32,
    /// Card bounds painted last frame, keyed by session. Written during
    /// prepaint, read by the next render.
    pub(super) cards: Rc<RefCell<HashMap<SessionId, ZoomRect>>>,
    /// This transition cross-fades instead of flying: Reduce Motion, a layout
    /// other than the mini windows, or a destination with no card.
    pub(super) crossfade: bool,
}

impl ZoomPresentation {
    /// Drop measured cards; the next painted grid measures them again.
    pub(super) fn forget_cards(&self) {
        self.cards.borrow_mut().clear();
    }

    /// The grid scrolled by `dy` since the cards were measured.
    pub(super) fn shift_cards(&self, dy: f32) {
        if dy != 0.0 {
            for card in self.cards.borrow_mut().values_mut() {
                card.y += dy;
            }
        }
    }
}

impl SessionSurfaces {
    pub(crate) fn set_page_region(&mut self, viewport: TerminalViewport, header: f32) {
        self.zoom.page = ZoomRect {
            x: viewport.x,
            y: viewport.y,
            width: viewport.width,
            height: viewport.height,
        };
        self.zoom.page_header = header;
    }

    /// Whether the transition layer, rather than the resting overview, paints.
    pub(super) fn zoom_painting(&self) -> bool {
        let zoom = &self.zoom.zoom;
        zoom.is_flying() || (zoom.is_tracking() && zoom.progress() > 0.0)
    }

    /// Stop any transition where it is, without animating. Used when another
    /// surface takes over (switcher, peek, window deactivation).
    pub(crate) fn reset_overview_zoom(&mut self, cx: &mut Context<Self>) {
        let visible = self
            .store
            .read()
            .expect("session store lock poisoned")
            .overview_state()
            .is_visible();
        let was_active = self.zoom.zoom.is_active();
        self.zoom.zoom.reset(if visible {
            Landing::Overview
        } else {
            Landing::Page
        });
        self.zoom.grip = None;
        if !visible {
            self.zoom.session = None;
        }
        if was_active {
            cx.notify();
        }
    }

    /// Only the mini-window cards share the pane's shape, so only they can be
    /// flown into; everything else (and Reduce Motion) cross-fades.
    fn zoom_can_fly(&self, cx: &Context<Self>) -> bool {
        !cx.reduce_motion() && self.overview_variant == OverviewVariant::Windows
    }

    /// The card's on-screen rect: as painted last frame, else as the layout
    /// will paint it at the grid's current scroll.
    fn zoom_slot(&self, id: &SessionId, window: &Window) -> Option<ZoomRect> {
        if let Some(card) = self.zoom.cards.borrow().get(id) {
            return Some(*card);
        }
        self.calm_card_estimate(id, window)
    }

    fn sync_slot_scale(&mut self, slot: Option<ZoomRect>) {
        if let Some(slot) = slot
            && self.zoom.page.width > 1.0
        {
            self.zoom
                .zoom
                .set_slot_scale(slot.width / self.zoom.page.width);
        }
    }

    fn zoom_frame(&self, slot: ZoomRect) -> ZoomRect {
        let zoom = &self.zoom.zoom;
        page_frame(
            self.zoom.page,
            slot,
            self.zoom.grip,
            zoom.finger_weight(),
            zoom.progress(),
            zoom.page_scale(),
        )
    }

    /// Follow a store change that opened or closed the overview without a
    /// pinch (⇧⌘O, Esc, a click, Return, the close button) with the
    /// non-interactive version of the same flight.
    pub(super) fn follow_overview_visibility(
        &mut self,
        visible: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let now = cx.background_executor().now();
        let reduced = cx.reduce_motion();
        let target = if visible {
            Landing::Overview
        } else {
            Landing::Page
        };
        if self.zoom.zoom.is_tracking() || self.zoom.zoom.heading() == Some(target) {
            // A pinch (or its release) already owns this change.
            return;
        }
        if self.zoom.zoom.is_flying() {
            // ⇧⌘O mid-flight turns the flight around from where it is.
            self.zoom.zoom.animate_to(target, now, reduced);
            return;
        }
        let selected = self
            .store
            .read()
            .expect("session store lock poisoned")
            .selected_session_id()
            .cloned();
        self.zoom.grip = None;
        self.zoom.session = selected.clone();
        match &selected {
            // Every open starts at the top, scrolled only as far as it takes
            // to show the card the page is about to land in.
            Some(id) => self.reveal_calm_card(id, window, visible),
            None if visible => self
                .overview_grid_scroll
                .set_offset(point(px(0.0), px(0.0))),
            None => {}
        }
        let slot = selected.as_ref().and_then(|id| self.zoom_slot(id, window));
        self.zoom.crossfade = !self.zoom_can_fly(cx) || slot.is_none();
        if visible {
            self.zoom.zoom.reset(Landing::Page);
            if let Some(id) = selected {
                self.request_screen(id, cx);
            }
        } else {
            // Leaving lands on whatever is now selected: the card that was
            // activated, or the page you came from after Esc.
            self.zoom.zoom.reset(Landing::Overview);
        }
        self.zoom.zoom.animate_to(target, now, reduced);
        self.sync_slot_scale(slot);
    }

    /// A trackpad pinch anywhere in the window. Returns whether the zoom took
    /// it; RootView plays the threshold haptic from `take_zoom_feedback`.
    pub(crate) fn overview_pinch(
        &mut self,
        event: &gpui::PinchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let now = cx.background_executor().now();
        let reduced = cx.reduce_motion();
        let fingers = (f32::from(event.position.x), f32::from(event.position.y));
        match event.phase {
            gpui::TouchPhase::Started => self.begin_zoom(fingers, now, window, cx),
            gpui::TouchPhase::Moved => {
                if !self.zoom.zoom.is_tracking() {
                    return false;
                }
                self.zoom.zoom.pinch(event.delta, now);
                if let Some(grip) = self.zoom.grip.as_mut() {
                    grip.fingers = fingers;
                }
                if self.zoom.zoom.progress() > 0.0 {
                    self.open_overview_under_pinch();
                }
                cx.notify();
                true
            }
            gpui::TouchPhase::Ended | gpui::TouchPhase::Cancelled => {
                let landing = if event.phase == gpui::TouchPhase::Ended {
                    self.zoom.zoom.release(now, reduced)
                } else {
                    self.zoom.zoom.cancel(now, reduced)
                };
                let Some(landing) = landing else {
                    return false;
                };
                self.settle_store_on(landing, cx);
                cx.notify();
                true
            }
        }
    }

    fn begin_zoom(
        &mut self,
        fingers: (f32, f32),
        now: std::time::Instant,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (overview_visible, switcher_visible, selected, focused) = {
            let store = self.store.read().expect("session store lock poisoned");
            (
                store.overview_state().is_visible(),
                store.switcher_state().is_visible(),
                store.selected_session_id().cloned(),
                store.overview_state().focused().cloned(),
            )
        };
        if self.peek.paint_visible() || switcher_visible {
            return false;
        }
        self.zoom.zoom.advance(now);
        let grabbing_flight = self.zoom.zoom.is_active() && self.zoom.session.is_some();
        if !grabbing_flight {
            let session = if overview_visible {
                // The card under the fingers, as in Safari; otherwise the
                // keyboard-focused card.
                let under = self
                    .zoom
                    .cards
                    .borrow()
                    .iter()
                    .find(|(_, card)| card.contains(fingers.0, fingers.1))
                    .map(|(id, _)| id.clone());
                under.or(focused)
            } else {
                selected
            };
            let Some(session) = session else {
                return false;
            };
            if !overview_visible {
                // The grid opens at the top, scrolled just far enough that
                // the card the page shrinks into is on screen.
                self.reveal_calm_card(&session, window, true);
            }
            self.zoom.session = Some(session);
            self.zoom.crossfade = !self.zoom_can_fly(cx);
        }
        let Some(session) = self.zoom.session.clone() else {
            return false;
        };
        let slot = self.zoom_slot(&session, window);
        if slot.is_none() {
            self.zoom.crossfade = true;
        }
        self.sync_slot_scale(slot);
        let at_rest = if overview_visible {
            Landing::Overview
        } else {
            Landing::Page
        };
        if !grabbing_flight {
            // At rest the presentation is wherever the store says it is.
            self.zoom.zoom.reset(at_rest);
        }
        // Take hold of the frame that is on screen right now.
        let on_screen = if !grabbing_flight && at_rest == Landing::Page {
            self.zoom.page
        } else {
            slot.map_or(self.zoom.page, |slot| self.zoom_frame(slot))
        };
        self.zoom.grip = Some(Grip::take(on_screen, fingers));
        self.zoom.zoom.begin(at_rest, now);
        // Snapshot cards for sessions that are not mounted start loading now,
        // while the page still covers the grid.
        self.request_screen(session, cx);
        cx.notify();
        true
    }

    /// The grid is behind the page from the first frame the page shrinks, so
    /// the store opens it then (keys and focus follow). A pinch that never
    /// shrinks the page never touches the store.
    fn open_overview_under_pinch(&mut self) {
        let mut store = self.store.write().expect("session store lock poisoned");
        if !store.overview_state().is_visible() {
            store.toggle_overview();
        }
    }

    /// Make the store agree with where a released pinch is heading, at
    /// release rather than at landing, so keys go to the right place at once.
    fn settle_store_on(&mut self, landing: Landing, cx: &mut Context<Self>) {
        let mut store = self.store.write().expect("session store lock poisoned");
        match landing {
            Landing::Overview => {
                if !store.overview_state().is_visible() {
                    store.toggle_overview();
                }
            }
            Landing::Page => {
                // Pinching a card open selects it at release, so the
                // workbench under the growing page is already that session.
                let selected = store.selected_session_id().cloned();
                let activate = self.zoom.session.clone().filter(|id| {
                    selected.as_ref() != Some(id) && store.sessions().contains_key(id)
                });
                if let Some(id) = activate.clone() {
                    store.select(id);
                }
                store.dismiss_overview();
                drop(store);
                if activate.is_some() {
                    cx.emit(TabPeekActivated);
                }
            }
        }
    }

    /// The window lost the gesture (deactivation, a modal surface): return
    /// to where the pinch started, exactly as a system cancel would.
    pub(crate) fn cancel_overview_pinch(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        if let Some(landing) = self.zoom.zoom.cancel(now, cx.reduce_motion()) {
            self.settle_store_on(landing, cx);
            cx.notify();
        }
    }

    pub(crate) fn take_zoom_feedback(&mut self) -> bool {
        self.zoom.zoom.take_feedback()
    }

    /// Advance a flight, and ask for the next frame only while one is in the
    /// air. A resting or finger-driven zoom schedules nothing.
    pub(super) fn advance_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        if self.zoom.zoom.advance(now) == Some(Landing::Page) {
            self.zoom.session = None;
            self.zoom.grip = None;
        }
        if self.zoom.zoom.is_flying() {
            window.request_animation_frame();
        }
    }

    /// Records where a card painted, for the next frame's flight.
    pub(super) fn card_probe(&self, id: SessionId) -> impl IntoElement {
        let cards = Rc::clone(&self.zoom.cards);
        gpui::canvas(
            move |bounds, _, _| {
                cards.borrow_mut().insert(
                    id,
                    ZoomRect {
                        x: f32::from(bounds.origin.x),
                        y: f32::from(bounds.origin.y),
                        width: f32::from(bounds.size.width),
                        height: f32::from(bounds.size.height),
                    },
                );
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    }

    /// This card is in flight above the grid, so its place paints nothing.
    pub(super) fn zoom_card_in_flight(&self, id: &SessionId) -> bool {
        self.zoom.session.as_ref() == Some(id) && self.zoom_painting() && !self.zoom.crossfade
    }

    /// The page's inside at progress 0: its title bar, its font, and its grid
    /// top-anchored under the title bar the way the pane paints it.
    fn page_pose(
        &self,
        element: Option<&TerminalElement>,
        font: &gpui::Font,
        window: &Window,
    ) -> CardPose {
        let size = self
            .store
            .read()
            .expect("session store lock poisoned")
            .preferences()
            .terminal_font_size;
        let rows = element.map_or(0, |element| element.grid_rows());
        let grid_height =
            f32::from(CellMetrics::measure(window.text_system(), font, px(size)).line_height)
                * f32::from(rows);
        // The flying card's area below the strip sits inside a one-point
        // border, like the card it becomes.
        let area = self.zoom.page.height - self.zoom.page_header - 2.0;
        CardPose {
            strip: self.zoom.page_header,
            font: size,
            grid_left: PAGE_GRID_LEFT,
            grid_bottom: (area - PAGE_GRID_TOP - grid_height).max(PAGE_GRID_MIN_BOTTOM),
            radius: 0.0,
        }
    }

    pub(super) fn render_overview_zoom(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors();
        let progress = self.zoom.zoom.progress();
        // Read last frame's card before the grid re-measures this frame.
        let session = self.zoom.session.clone().and_then(|id| {
            self.store
                .read()
                .expect("session store lock poisoned")
                .sessions()
                .get(&id)
                .cloned()
        });
        let slot = session
            .as_ref()
            .and_then(|session| self.zoom_slot(&session.id, window));
        self.sync_slot_scale(slot);
        let flying = !self.zoom.crossfade && session.is_some() && slot.is_some();
        let grid = self.render_overview(window, cx);
        let mut layer = div().id("overview-zoom").absolute().inset_0().size_full();
        let shield = div()
            .id("overview-zoom-shield")
            .absolute()
            .inset_0()
            .occlude();
        let (Some(session), Some(slot), true) = (session, slot, flying) else {
            // Reduce Motion (or nowhere to land): the grid simply fades over
            // the workbench, following the fingers.
            return layer
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .size_full()
                        .opacity(progress.clamp(0.0, 1.0))
                        .child(grid),
                )
                .child(shield)
                .into_any_element();
        };
        let (theme, focused) = {
            let store = self.store.read().expect("session store lock poisoned");
            (
                crate::app_theme::terminal_theme_in(&store),
                store.overview_state().focused() == Some(&session.id),
            )
        };
        let page = self.zoom.page;
        let frame = self.zoom_frame(slot);
        let geometry = self.calm_card_geometry(window);
        let element = self.card_terminal(&session.id).cloned();
        let page_pose = self.page_pose(element.as_ref(), &geometry.font, window);
        let pose = card_pose(page_pose, geometry.pose, progress);
        let mini = miniature_box(
            element.as_ref(),
            frame.width,
            (frame.height - pose.strip - 2.0).max(0.0),
            pose.grid_left,
            pose.grid_bottom,
            pose.font,
            &geometry.font,
            theme,
            session.hibernation.is_some(),
            window,
        );
        // The flying page is the card: the same builder the grid uses, at the
        // pose between the two, so progress 1 paints the resting card itself.
        let card = self.calm_window_card(
            &session,
            pose.strip,
            pose.radius,
            progress,
            focused,
            mini.into_any_element(),
            theme,
            colors,
            window,
            cx,
        );
        let flying_page = div()
            .absolute()
            .left(px(frame.x))
            .top(px(frame.y))
            .w(px(frame.width))
            .h(px(frame.height))
            .child(card);
        layer = layer
            // The workbench under the shrinking page is revealed as the
            // overview's desk, never as a second copy of the terminal.
            .child(
                div()
                    .absolute()
                    .left(px(page.x))
                    .top(px(page.y))
                    .w(px(page.width))
                    .h(px(page.height))
                    .bg(self.calm_desk()),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .opacity(grid_alpha(progress))
                    .child(grid),
            )
            .child(flying_page);
        // The grid is a picture until the flight lands; clicks mid-flight
        // would act on cards that are still moving.
        layer.child(shield).into_any_element()
    }
}
