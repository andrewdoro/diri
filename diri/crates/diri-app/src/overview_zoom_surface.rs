//! Painting and input for the Safari-style zoom between a session and the
//! overview grid. The state machine lives in `crate::overview_zoom`; this file
//! only maps it onto the store and onto elements.
use super::*;
use crate::overview_zoom::{
    Grip, Landing, OverviewZoom, ZoomRect, estimated_slot, grid_alpha, live_page_alpha, page_frame,
};
use std::cell::RefCell;
use std::rc::Rc;

/// Everything the zoom needs besides the pure state machine.
#[derive(Default)]
pub(super) struct ZoomPresentation {
    pub(super) zoom: OverviewZoom,
    /// The session playing the page: the one you pinched away from, or the
    /// thumbnail you pinched (or clicked) open.
    pub(super) session: Option<SessionId>,
    pub(super) grip: Option<Grip>,
    /// The workbench card in window coordinates, supplied by RootView.
    pub(super) page: ZoomRect,
    /// Thumbnail bounds painted last frame, keyed by session. Written during
    /// prepaint, read by the next render.
    pub(super) slots: Rc<RefCell<HashMap<SessionId, ZoomRect>>>,
    /// This transition cross-fades instead of flying: Reduce Motion, the list
    /// mode, or a destination that was never painted.
    pub(super) crossfade: bool,
    pub(super) frame_pending: bool,
}

impl SessionSurfaces {
    pub(crate) fn set_page_region(&mut self, left: f32, top: f32, width: f32, height: f32) {
        self.zoom.page = ZoomRect {
            x: left,
            y: top,
            width,
            height,
        };
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

    fn zoom_slot(&self, id: &SessionId, viewport_width: f32) -> Option<ZoomRect> {
        if let Some(slot) = self.zoom.slots.borrow().get(id) {
            return Some(*slot);
        }
        let mut store = self.store.write().expect("session store lock poisoned");
        let sessions = store.ordered_sessions();
        let state = store.overview_state();
        let index = state
            .visible_sessions(&sessions)
            .position(|s| &s.id == id)?;
        Some(estimated_slot(
            index,
            overview_columns(viewport_width),
            viewport_width,
        ))
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

    fn overview_mode_is_grid(&self) -> bool {
        self.store
            .read()
            .expect("session store lock poisoned")
            .overview_state()
            .mode()
            == OverviewMode::Grid
    }

    /// Scroll the gallery so the page's own thumbnail is on screen, which is
    /// where it is about to land.
    pub(super) fn reveal_zoom_slot(&self, viewport_width: f32) {
        let Some(id) = self.zoom.session.as_ref() else {
            self.overview_grid_scroll.scroll_to_item(0);
            return;
        };
        let mut store = self.store.write().expect("session store lock poisoned");
        let sessions = store.ordered_sessions();
        let index = store
            .overview_state()
            .visible_sessions(&sessions)
            .position(|s| &s.id == id)
            .unwrap_or(0);
        self.overview_grid_scroll
            .scroll_to_item(index / overview_columns(viewport_width));
    }

    /// Follow a store change that opened or closed the overview without a
    /// pinch (⇧⌘O, Esc, a click, Return, the close button) with the
    /// non-interactive version of the same flight.
    pub(super) fn follow_overview_visibility(
        &mut self,
        visible: bool,
        viewport_width: f32,
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
        if visible {
            self.zoom.session = selected;
            self.zoom.crossfade = reduced || !self.overview_mode_is_grid();
            self.zoom.zoom.reset(Landing::Page);
            self.zoom.zoom.animate_to(Landing::Overview, now, reduced);
            if let Some(id) = self.zoom.session.clone() {
                self.request_screen(id, cx);
            }
        } else {
            // Leaving lands on whatever is now selected: the card that was
            // activated, or the page you came from after Esc.
            let measured = selected
                .as_ref()
                .is_some_and(|id| self.zoom.slots.borrow().contains_key(id));
            self.zoom.session = selected;
            self.zoom.crossfade = reduced || !self.overview_mode_is_grid() || !measured;
            self.zoom.zoom.reset(Landing::Overview);
            self.zoom.zoom.animate_to(Landing::Page, now, reduced);
        }
        let slot = self
            .zoom
            .session
            .clone()
            .and_then(|id| self.zoom_slot(&id, viewport_width));
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
        let viewport_width = f32::from(window.viewport_size().width);
        let fingers = (f32::from(event.position.x), f32::from(event.position.y));
        match event.phase {
            gpui::TouchPhase::Started => self.begin_zoom(fingers, now, viewport_width, cx),
            gpui::TouchPhase::Moved => {
                if !self.zoom.zoom.is_tracking() {
                    return false;
                }
                self.zoom.zoom.pinch(event.delta, now);
                if let Some(grip) = self.zoom.grip.as_mut() {
                    grip.fingers = fingers;
                }
                if self.zoom.zoom.progress() > 0.0 {
                    self.open_overview_under_pinch(viewport_width);
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
        viewport_width: f32,
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
                // The thumbnail under the fingers, as in Safari; otherwise the
                // keyboard-focused card.
                let under = self
                    .zoom
                    .slots
                    .borrow()
                    .iter()
                    .find(|(_, slot)| slot.contains(fingers.0, fingers.1))
                    .map(|(id, _)| id.clone());
                under.or(focused)
            } else {
                selected
            };
            let Some(session) = session else {
                return false;
            };
            self.zoom.session = Some(session);
            self.zoom.crossfade = cx.reduce_motion() || !self.overview_mode_is_grid();
        }
        let Some(session) = self.zoom.session.clone() else {
            return false;
        };
        let slot = self.zoom_slot(&session, viewport_width);
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
        // Fetch the page's card preview now, while the live terminal still
        // covers it, so the hand-over never shows a loading card.
        self.request_screen(session, cx);
        cx.notify();
        true
    }

    /// The grid is behind the page from the first frame the page shrinks, so
    /// the store opens it then (keys and focus follow). A pinch that never
    /// shrinks the page never touches the store.
    fn open_overview_under_pinch(&mut self, viewport_width: f32) {
        let mut store = self.store.write().expect("session store lock poisoned");
        if !store.overview_state().is_visible() {
            store.toggle_overview();
            drop(store);
            self.reveal_zoom_slot(viewport_width);
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
                // Pinching a thumbnail open selects it at release, so the
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

    /// Advance a flight and keep frames coming only while one is in the air.
    pub(super) fn advance_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        if self.zoom.zoom.advance(now) == Some(Landing::Page) {
            self.zoom.session = None;
            self.zoom.grip = None;
        }
        if self.zoom.zoom.is_flying() && !self.zoom.frame_pending {
            self.zoom.frame_pending = true;
            let entity = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.zoom.frame_pending = false;
                    if this.zoom.zoom.is_flying() {
                        cx.notify();
                    }
                });
            });
        }
    }

    /// The live read-only terminal at `scale` of the terminal font, inset like
    /// the real grid so the text sits where it sat on the page. It only covers
    /// the first moment of a pinch, fading into the card preview underneath.
    pub(super) fn page_miniature(
        &self,
        session: &SessionRecord,
        scale: f32,
        colors: SemanticColors,
    ) -> AnyElement {
        let font_size = self
            .store
            .read()
            .expect("session store lock poisoned")
            .preferences()
            .terminal_font_size;
        let left = crate::terminal_pane::GRID_HORIZONTAL_PADDING * scale;
        let top =
            (diri_ui::Metrics::TITLE_BAR + crate::terminal_pane::GRID_VERTICAL_PADDING) * scale;
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(colors.work_surface_nested())
            .child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .right_0()
                    .bottom_0()
                    .child(self.render_grid_or_logo(
                        session,
                        (64.0 * scale).max(20.0),
                        (font_size * scale).max(1.0),
                        colors,
                    )),
            )
            .into_any_element()
    }

    /// Records where a thumbnail painted, for the next frame's flight.
    pub(super) fn slot_probe(&self, id: SessionId) -> impl IntoElement {
        let slots = Rc::clone(&self.zoom.slots);
        gpui::canvas(
            move |bounds, _, _| {
                slots.borrow_mut().insert(
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

    /// This card is in flight above the grid, so its slot paints nothing.
    pub(super) fn zoom_card_in_flight(&self, id: &SessionId) -> bool {
        self.zoom.session.as_ref() == Some(id) && self.zoom_painting() && !self.zoom.crossfade
    }

    pub(super) fn render_overview_zoom(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let viewport_width = f32::from(window.viewport_size().width);
        let colors = self.colors();
        let progress = self.zoom.zoom.progress();
        // Read last frame's slot before the grid re-measures this frame.
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
            .and_then(|session| self.zoom_slot(&session.id, viewport_width));
        self.sync_slot_scale(slot);
        let flying = !self.zoom.crossfade && session.is_some() && slot.is_some();
        let grid = self.render_overview(window, cx);
        let mut layer = div().id("overview-zoom").absolute().inset_0().size_full();
        if !flying {
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
                .child(
                    div()
                        .id("overview-zoom-shield")
                        .absolute()
                        .inset_0()
                        .occlude(),
                )
                .into_any_element();
        }
        let (Some(session), Some(slot)) = (session, slot) else {
            return layer.into_any_element();
        };
        let page = self.zoom.page;
        let frame = self.zoom_frame(slot);
        let page_scale = frame.width / page.width.max(1.0);
        let card_scale = frame.width / slot.width.max(1.0);
        let p = progress.clamp(0.0, 1.0);
        let live = live_page_alpha(progress);
        // The flying page *is* the grid card: the same preview ⇧⌘O paints,
        // at the size it is flying at, so landing is the card exactly. Only
        // the first moments show the live terminal, fading into that card.
        let card = self.overview_preview(&session, card_scale, colors, cx);
        let mut flying_page = div()
            .absolute()
            .left(px(frame.x))
            .top(px(frame.y))
            .w(px(frame.width))
            .h(px(frame.height))
            .rounded(px(Radius::ROW * p))
            .overflow_hidden()
            .bg(colors.background)
            .border_1()
            .border_color(colors.primary.alpha(0.075 * p))
            .when(session.hibernation.is_some(), |page| {
                page.opacity(1.0 - 0.32 * p)
            })
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.18 * p),
                offset: point(px(0.0), px(6.0 * p)),
                blur_radius: px(18.0 * p),
                spread_radius: px(0.0),
                inset: false,
            }])
            .child(card);
        if live > 0.0 {
            flying_page = flying_page.child(
                div()
                    .absolute()
                    .inset_0()
                    .opacity(live)
                    .child(self.page_miniature(&session, page_scale, colors)),
            );
        }
        layer = layer
            // The workbench under the shrinking page is revealed as grid
            // background, never as a second copy of the terminal.
            .child(
                div()
                    .absolute()
                    .left(px(page.x))
                    .top(px(page.y))
                    .w(px(page.width))
                    .h(px(page.height))
                    .bg(colors.background),
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
        layer
            .child(
                div()
                    .id("overview-zoom-shield")
                    .absolute()
                    .inset_0()
                    .occlude(),
            )
            .into_any_element()
    }
}
