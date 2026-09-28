//! Design exploration (phase 1): calm, Safari-like Session Overview variants.
//! Cards are true miniatures of each session's terminal pane: the resident
//! grid painted at a scaled font size inside a box with the pane's own aspect
//! ratio. Only the prototype layouts live here; the current overview stays the
//! default until one variant is chosen.
use super::*;
use diri_term::metrics::CellMetrics;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OverviewVariant {
    #[default]
    Current,
    /// A: Safari-literal. Two large cards, a title strip attached above.
    Safari,
    /// B: three columns, the title sits below the miniature.
    Gallery,
    /// C: miniature windows. The title bar is part of the pane's own surface,
    /// grouped by project with a quiet label.
    Windows,
}

impl OverviewVariant {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_env(value: &str) -> Self {
        match value {
            "a" | "safari" => Self::Safari,
            "b" | "gallery" => Self::Gallery,
            "c" | "windows" => Self::Windows,
            _ => Self::Current,
        }
    }
}

/// Font size and padding of the live pane the miniature is scaled from.
const PANE_FONT: f32 = 13.0;
const PANE_PAD: f32 = 12.0;
const FONT_STEP: f32 = 0.25;

fn mix(a: gpui::Rgba, b: gpui::Rgba, t: f32) -> gpui::Rgba {
    gpui::Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
}

/// Width-fitted miniature: the largest font on the ladder whose columns fit,
/// and the box height that grid needs (line heights round to whole pixels at
/// small sizes, so the height follows the metrics rather than pane aspect).
fn miniature_geometry(
    window: &Window,
    font: &gpui::Font,
    pane: CellMetrics,
    card_width: f32,
    cols: u16,
    rows: u16,
) -> (f32, f32) {
    let scale = card_width / (f32::from(pane.cell_width) * f32::from(cols) + 2.0 * PANE_PAD);
    let pad = PANE_PAD * scale;
    let inner_w = card_width - 2.0 * pad;
    let mut size = ((PANE_FONT * scale) / FONT_STEP).ceil() * FONT_STEP;
    loop {
        let m = CellMetrics::measure(window.text_system(), font, px(size));
        if f32::from(m.cell_width) * f32::from(cols) <= inner_w + 0.5 || size <= FONT_STEP {
            // Half a pixel of slack: rows are floored from the painted height.
            return (
                size,
                (f32::from(m.line_height) * f32::from(rows) + 2.0 * pad + 0.5).ceil(),
            );
        }
        size -= FONT_STEP;
    }
}

struct Layout {
    columns: usize,
    margin: f32,
    gutter: f32,
    row_gap: f32,
    radius: f32,
    strip: f32,
}

impl SessionSurfaces {
    fn fleet_grid(&self) -> (u16, u16) {
        self.resident_previews
            .values()
            .map(|element| (element.grid_cols(), element.grid_rows()))
            .find(|(cols, rows)| *cols > 0 && *rows > 0)
            .unwrap_or((120, 36))
    }

    pub(super) fn render_calm_overview(
        &mut self,
        variant: OverviewVariant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (sessions, state, theme, projects) = {
            let mut store = self.store.write().expect("session store lock poisoned");
            let projects: HashMap<_, _> = store
                .projects()
                .values()
                .map(|project| (project.id.clone(), project.name.clone()))
                .collect();
            (
                store.ordered_sessions(),
                store.overview_state().clone(),
                crate::app_theme::terminal_theme_in(&store),
                projects,
            )
        };
        let colors = self.colors();
        let dark = colors.appearance == diri_ui::Appearance::Dark;
        let black = gpui::rgba(0x000000ff);
        let desk = if dark {
            mix(theme.background, black, 0.42)
        } else {
            mix(theme.background, black, 0.075)
        };
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width);
        let layout = match variant {
            OverviewVariant::Safari => Layout {
                columns: if width >= 1800.0 { 3 } else { 2 },
                margin: 88.0,
                gutter: 64.0,
                row_gap: 52.0,
                radius: 10.0,
                strip: 28.0,
            },
            OverviewVariant::Gallery => Layout {
                columns: if width >= 1200.0 {
                    3
                } else if width >= 760.0 {
                    2
                } else {
                    1
                },
                margin: 56.0,
                gutter: 36.0,
                row_gap: 30.0,
                radius: 9.0,
                strip: 34.0,
            },
            OverviewVariant::Windows | OverviewVariant::Current => Layout {
                columns: ((width - 112.0) / 520.0).round().clamp(1.0, 4.0) as usize,
                margin: 56.0,
                gutter: 40.0,
                row_gap: 40.0,
                radius: 10.0,
                strip: 26.0,
            },
        };
        self.store
            .write()
            .unwrap()
            .set_overview_columns(layout.columns);
        let card_width =
            (width - 2.0 * layout.margin - layout.gutter * (layout.columns as f32 - 1.0))
                / layout.columns as f32;

        let font = crate::fonts::terminal_font();
        let pane = CellMetrics::measure(window.text_system(), &font, px(PANE_FONT));

        let visible: Vec<SessionRecord> = state.visible_sessions(&sessions).cloned().collect();
        let mut groups: Vec<(Option<String>, Vec<SessionRecord>)> = Vec::new();
        if variant == OverviewVariant::Windows {
            for session in visible {
                let name = projects
                    .get(&session.project_id)
                    .cloned()
                    .unwrap_or_else(|| "Sessions".to_owned());
                match groups.last_mut() {
                    Some((Some(last), items)) if *last == name => items.push(session),
                    _ => groups.push((Some(name), vec![session])),
                }
            }
        } else {
            groups.push((None, visible));
        }
        let show_new = state.query().is_empty()
            && matches!(variant, OverviewVariant::Safari | OverviewVariant::Windows);

        let mut body = div()
            .id("calm-overview-scroll")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .px(px(layout.margin))
            .pt(px(if variant == OverviewVariant::Safari {
                30.0
            } else {
                22.0
            }))
            .pb(px(56.0))
            .gap(px(layout.row_gap))
            .track_scroll(&self.overview_grid_scroll)
            .overflow_y_scroll();
        let group_count = groups.len();
        for (group_index, (label, items)) in groups.into_iter().enumerate() {
            let mut cells: Vec<AnyElement> = items
                .iter()
                .map(|session| {
                    self.calm_card(
                        variant, session, &state, &layout, card_width, pane, &font, theme, colors,
                        desk, window, cx,
                    )
                })
                .collect();
            if show_new && group_index + 1 == group_count {
                cells.push(self.calm_new_card(variant, &layout, card_width, pane, colors, window));
            }
            let mut section = div().flex().flex_none().flex_col().gap(px(layout.row_gap));
            if let Some(label) = label {
                section = section.gap(px(14.0)).child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.secondary)
                        .child(label),
                );
            }
            let mut rows = div().flex().flex_none().flex_col().gap(px(layout.row_gap));
            let mut cells = cells.into_iter().peekable();
            while cells.peek().is_some() {
                let mut row = div().flex().flex_none().gap(px(layout.gutter));
                let mut count = 0;
                for cell in cells.by_ref().take(layout.columns) {
                    row = row.child(div().w(px(card_width)).flex_none().child(cell));
                    count += 1;
                }
                for _ in count..layout.columns {
                    row = row.child(div().w(px(card_width)).flex_none());
                }
                rows = rows.child(row);
            }
            body = body.child(section.child(rows));
        }

        let search = self.calm_search(&state, colors, cx);

        let content = div()
            .id("overview-content")
            .debug_selector(|| "OVERVIEW_CONTENT".into())
            .absolute()
            .inset_0()
            .size_full()
            .flex()
            .flex_col()
            .pt(px(42.0))
            .bg(desk)
            .overflow_hidden()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(body)
            .child(search);

        div()
            .id("overview-scrim")
            .absolute()
            .inset_0()
            .size_full()
            .bg(desk)
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(content)
            .into_any_element()
    }

    fn calm_search(
        &self,
        state: &crate::switcher::SessionOverviewState,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let empty = state.query().is_empty();
        div()
            .id("overview-search")
            .absolute()
            .top(px(9.0))
            .right(px(14.0))
            .w(px(if empty { 168.0 } else { 220.0 }))
            .h(px(26.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .rounded(px(Radius::ROW))
            .bg(colors.primary.alpha(0.07))
            .border_1()
            .border_color(colors.primary.alpha(0.06))
            .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus_handle, cx)))
            .child(sf_symbol("magnifyingglass", 12.0, colors.tertiary))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(12.5))
                    .text_color(if empty {
                        colors.tertiary
                    } else {
                        colors.primary
                    })
                    .child(if empty {
                        "Search".to_owned()
                    } else {
                        state.query().to_owned()
                    }),
            )
            .into_any_element()
    }

    /// The miniature: the pane's grid at a scaled font inside a box that has
    /// the pane's aspect ratio, so the live pane can later zoom into it.
    fn calm_miniature(
        &self,
        session: &SessionRecord,
        card_width: f32,
        pane: CellMetrics,
        font: &gpui::Font,
        theme: diri_term::theme::TermTheme,
        window: &Window,
        cx: &Context<Self>,
    ) -> (AnyElement, f32) {
        let live = self
            .resident_previews
            .get(&session.id)
            .or_else(|| self.screen_grids.get(&session.id));
        // Every card has the shape of the pane you work in and one text size,
        // whatever size each session's own terminal happens to be. A session
        // wider or taller than that is cropped, keeping its bottom rows, where
        // the prompt and the latest output are.
        let (cols, rows) = self.fleet_grid();
        let (size, height) = miniature_geometry(window, font, pane, card_width, cols, rows);
        let pad =
            PANE_PAD * card_width / (f32::from(pane.cell_width) * f32::from(cols) + 2.0 * PANE_PAD);
        let mini_metrics = CellMetrics::measure(window.text_system(), font, px(size));
        let mut mini = div()
            .relative()
            .flex_none()
            .w(px(card_width))
            .h(px(height))
            .overflow_hidden()
            .bg(theme.background);
        if live.is_none()
            && !self.screens.contains_key(&session.id)
            && !self.screen_requests.contains_key(&session.id)
        {
            // Only cards that are actually on screen ask for their screen;
            // a completion repaints and lets the next ones in.
            let weak = cx.entity().downgrade();
            let id = session.id.clone();
            mini = mini.child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        if bounds.intersects(&window.content_mask().bounds) {
                            let id = id.clone();
                            let weak = weak.clone();
                            cx.defer(move |cx| {
                                let _ = weak.update(cx, |this, cx| this.request_screen(id, cx));
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        }
        if let Some(element) = live {
            let own_cols = element.grid_cols().max(1);
            let own_rows = element.grid_rows().max(1);
            mini = mini.child(
                div()
                    .absolute()
                    .left(px(pad))
                    .bottom(px(pad))
                    .w(mini_metrics.cell_width * f32::from(own_cols))
                    .h(mini_metrics.line_height * f32::from(own_rows))
                    .child(
                        element
                            .clone()
                            .font(font.clone())
                            .font_size(px(size))
                            .theme(theme),
                    ),
            );
        }
        if session.hibernation.is_some() {
            mini = mini.opacity(0.55);
        }
        (mini.into_any_element(), height)
    }

    #[allow(clippy::too_many_arguments)]
    fn calm_card(
        &mut self,
        variant: OverviewVariant,
        session: &SessionRecord,
        state: &crate::switcher::SessionOverviewState,
        layout: &Layout,
        card_width: f32,
        pane: CellMetrics,
        font: &gpui::Font,
        theme: diri_term::theme::TermTheme,
        colors: SemanticColors,
        desk: gpui::Rgba,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dark = colors.appearance == diri_ui::Appearance::Dark;
        let focused = state.focused() == Some(&session.id);
        let id = session.id.clone();
        let glyph = self.status_glyph(session, 12.0, colors, window, cx);
        let (mini, _) = self.calm_miniature(session, card_width, pane, font, theme, window, cx);
        let title = div()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .child(display_title(session));
        let black = gpui::rgba(0x000000ff);
        let ring = if dark {
            gpui::rgba(0xffffffd9)
        } else {
            gpui::rgba(0x0a84ffff)
        };
        let shadow = vec![BoxShadow {
            color: gpui::hsla(0.0, 0.0, 0.0, if dark { 0.45 } else { 0.12 }),
            offset: point(px(0.0), px(if dark { 10.0 } else { 6.0 })),
            blur_radius: px(if dark { 30.0 } else { 18.0 }),
            spread_radius: px(0.0),
            inset: false,
        }];
        let hairline = colors.primary.alpha(if dark { 0.10 } else { 0.09 });

        let card = match variant {
            OverviewVariant::Safari => {
                let strip_bg = if dark {
                    mix(theme.background, black, 0.45)
                } else {
                    mix(theme.background, black, 0.055)
                };
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(layout.radius))
                    .overflow_hidden()
                    .border_1()
                    .border_color(hairline)
                    .shadow(shadow)
                    .child(
                        div()
                            .flex_none()
                            .h(px(layout.strip))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(6.0))
                            .px(px(12.0))
                            .bg(strip_bg)
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.primary.alpha(0.88))
                            .child(glyph)
                            .child(title),
                    )
                    .child(mini)
            }
            OverviewVariant::Gallery => div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .rounded(px(layout.radius))
                        .overflow_hidden()
                        .border_1()
                        .border_color(hairline)
                        .shadow(shadow)
                        .child(mini),
                )
                .child(
                    div()
                        .flex_none()
                        .h(px(layout.strip))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .px(px(2.0))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.primary.alpha(0.82))
                        .child(glyph)
                        .child(title),
                ),
            _ => div()
                .flex()
                .flex_col()
                .rounded(px(layout.radius))
                .overflow_hidden()
                .border_1()
                .border_color(hairline)
                .shadow(shadow)
                .bg(theme.background)
                .child(
                    div()
                        .flex_none()
                        .h(px(layout.strip))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(10.0))
                        .border_b_1()
                        .border_color(colors.primary.alpha(0.07))
                        .text_size(px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.secondary)
                        .child(glyph)
                        .child(title),
                )
                .child(mini),
        };

        // One clean ring, drawn outside the card so it never covers content.
        let inset = 4.0;
        let ring_radius = layout.radius + inset;
        let ringed = div().relative().child(card).child(
            div()
                .absolute()
                .top(px(-inset))
                .left(px(-inset))
                .right(px(-inset))
                .bottom(px(if variant == OverviewVariant::Gallery {
                    layout.strip - inset
                } else {
                    -inset
                }))
                .rounded(px(ring_radius))
                .border_2()
                .border_color(if focused { ring } else { ring.alpha(0.0) }),
        );
        let _ = desk;
        div()
            .id(SharedString::from(format!("overview-card-{}", id.0)))
            .debug_selector(|| format!("OVERVIEW_CARD_{}", id.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let mut store = this.store.write().expect("session store lock poisoned");
                if event.modifiers().platform {
                    store.toggle_overview_selection(id.clone());
                } else {
                    store.activate_overview_session(id.clone());
                }
                drop(store);
                cx.notify();
            }))
            .child(ringed)
            .into_any_element()
    }

    fn calm_new_card(
        &self,
        variant: OverviewVariant,
        layout: &Layout,
        card_width: f32,
        pane: CellMetrics,
        colors: SemanticColors,
        window: &Window,
    ) -> AnyElement {
        let (cols, rows) = self.fleet_grid();
        let font = crate::fonts::terminal_font();
        let (_, mini_height) = miniature_geometry(window, &font, pane, card_width, cols, rows);
        let height = mini_height
            + if variant == OverviewVariant::Gallery {
                0.0
            } else {
                layout.strip + 2.0
            };
        div()
            .id("overview-new-session")
            .w(px(card_width))
            .h(px(height))
            .rounded(px(layout.radius))
            .bg(colors.primary.alpha(0.05))
            .border_1()
            .border_color(colors.primary.alpha(0.06))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(colors.primary.alpha(0.08)))
            .child(sf_symbol_weighted(
                "plus",
                if variant == OverviewVariant::Safari {
                    44.0
                } else {
                    30.0
                },
                SymbolWeight::Regular,
                colors.primary.alpha(0.45),
            ))
            .into_any_element()
    }
}
