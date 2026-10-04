// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! Drawing the API surface. Ely's builder, response viewer, environment
//! selector and collection tree are laid out for a narrow right panel: one
//! column, a mode strip on top, the request above its answer.
//!
//! Every fill is a translucent tint of the foreground (as in `details_ui`), so
//! the surface reads on whatever panel colour the inspector paints.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use diri_ui::{IconName, Ink, Palette, Radius, SemanticColors, Typo};
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, HighlightStyle, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, StyledText, Window, deferred, div, prelude::*, px,
    relative, uniform_list,
};

use super::json::{PrettyJson, Token};
use super::model::{self, Auth, BodyKind, Method, Pair, Resolved, TreeRow};
use super::{ApiClient, Column, Field, Mode, Outcome, RequestTab, ResponseTab, ResponseView};
use crate::details_ui::{self, icon};

const ROW_HEIGHT: f32 = 18.0;
const FIELD_HEIGHT: f32 = 26.0;
const INSET: f32 = 10.0;
const ACCENT: gpui::Rgba = gpui::Rgba {
    r: 0.31,
    g: 0.514,
    b: 0.945,
    a: 0.8,
};

/// The method's tag colour, as API tools conventionally paint them.
pub(super) fn method_color(method: Method, colors: SemanticColors) -> gpui::Rgba {
    match method {
        Method::Get => Ink::FRESH,
        Method::Post => Ink::ATTENTION,
        Method::Put => Palette::GEMINI_BLUE,
        Method::Patch => details_ui::MERGED,
        Method::Delete => Ink::DANGER,
        Method::Head | Method::Options => colors.secondary,
    }
}

/// The status class's tone: success, a redirect, the client's fault, the
/// server's (Ely's `status_tone`).
pub(super) fn status_color(status: u16, colors: SemanticColors) -> gpui::Rgba {
    match status {
        200..=299 => Ink::FRESH,
        300..=399 => Palette::GEMINI_BLUE,
        400..=499 => Ink::ATTENTION,
        500..=599 => Ink::DANGER,
        _ => colors.secondary,
    }
}

fn token_color(token: Token, colors: SemanticColors) -> gpui::Rgba {
    match token {
        Token::Key => match colors.appearance {
            diri_ui::Appearance::Dark => gpui::rgba(0x9cdcfeff),
            diri_ui::Appearance::Light => gpui::rgba(0x0451a5ff),
        },
        Token::String => match colors.appearance {
            diri_ui::Appearance::Dark => gpui::rgba(0xce9178ff),
            diri_ui::Appearance::Light => gpui::rgba(0xa31515ff),
        },
        Token::Number => match colors.appearance {
            diri_ui::Appearance::Dark => gpui::rgba(0xb5cea8ff),
            diri_ui::Appearance::Light => gpui::rgba(0x098658ff),
        },
        Token::Literal => match colors.appearance {
            diri_ui::Appearance::Dark => gpui::rgba(0x569cd6ff),
            diri_ui::Appearance::Light => gpui::rgba(0x0000ffff),
        },
        Token::Punctuation => colors.tertiary,
    }
}

fn mono() -> &'static str {
    crate::fonts::mono_family()
}

/// A small text button in the quiet style.
fn text_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    colors: SemanticColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(22.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .rounded(px(Radius::CHIP))
        .cursor_pointer()
        .hover(move |button| button.bg(colors.primary.alpha(0.07)))
        .text_size(px(11.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors.secondary)
        .child(label.into())
}

fn method_tag(method: Method, colors: SemanticColors) -> AnyElement {
    div()
        .flex_none()
        .min_w(px(34.0))
        .font_family(mono())
        .text_size(px(9.5))
        .font_weight(FontWeight::BOLD)
        .text_color(method_color(method, colors))
        .child(method.name())
        .into_any_element()
}

/// `{{name}}` placeholders tinted: known names in the accent, missing ones
/// in the warning ink.
fn placeholder_highlights(
    text: &str,
    known: &dyn Fn(&str) -> bool,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut highlights = Vec::new();
    let mut offset = 0;
    while let Some(open) = text[offset..].find("{{") {
        let start = offset + open;
        let Some(close) = text[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + close + 2;
        let name = text[start + 2..end - 2].trim();
        let color = if known(name) {
            Palette::GEMINI_BLUE
        } else {
            Ink::ATTENTION
        };
        highlights.push((
            start..end,
            HighlightStyle {
                color: Some(color.into()),
                ..HighlightStyle::default()
            },
        ));
        offset = end;
    }
    highlights
}

impl ApiClient {
    fn known_variable(&self, name: &str, cx: &gpui::App) -> bool {
        self.library
            .read(cx)
            .active_environment()
            .is_some_and(|environment| environment.lookup(name).is_some())
    }

    /// A one-line or multi-line text field. Unfocused it shows its text (or a
    /// placeholder, or dots for a secret); focused it shows the editor with
    /// its caret and selection.
    fn render_field(
        &self,
        field: Field,
        placeholder: &'static str,
        masked: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = self.colors;
        let focused = self.focused_field() == Some(field);
        let multiline = field == Field::Body;
        let content: AnyElement = match &self.field {
            Some((current, editor)) if *current == field => crate::navigation::query_label(editor),
            _ => {
                let text = self.field_text(field, cx);
                if text.is_empty() {
                    div()
                        .text_color(colors.tertiary)
                        .child(placeholder)
                        .into_any_element()
                } else if masked {
                    div()
                        .child("•".repeat(text.chars().count().min(24)))
                        .into_any_element()
                } else if text.contains("{{") {
                    let highlights =
                        placeholder_highlights(&text, &|name| self.known_variable(name, cx));
                    StyledText::new(text)
                        .with_highlights(highlights)
                        .into_any_element()
                } else {
                    div().child(text).into_any_element()
                }
            }
        };
        div()
            .id(SharedString::from(format!("api-field-{field:?}")))
            .min_w(px(0.0))
            .when(!multiline, |field| {
                field
                    .h(px(FIELD_HEIGHT))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
            })
            .when(multiline, |field| field.min_h(px(120.0)).py(px(6.0)))
            .px(px(7.0))
            .rounded(px(Radius::CHIP))
            .bg(colors.primary.alpha(if focused { 0.06 } else { 0.035 }))
            .border_1()
            .border_color(if focused {
                ACCENT
            } else {
                colors.primary.alpha(0.06)
            })
            .text_size(px(11.5))
            .text_color(colors.primary)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.focus_field(field, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(content)
    }

    // MARK: Header strip

    fn render_modes(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let history = self.library.read(cx).project.history.len();
        let mode = |id: &'static str, label: &'static str, target: Mode, count: Option<usize>| {
            details_ui::segment(
                id,
                label,
                count.filter(|count| *count > 0),
                self.mode == target,
                colors,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.commit_field();
                this.mode = target;
                cx.notify();
            }))
        };
        div()
            .flex_none()
            .h(px(40.0))
            .px(px(INSET))
            .flex()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(colors.primary.alpha(0.065))
            .child(
                details_ui::segmented_track(colors)
                    .child(mode("api-mode-request", "Request", Mode::Request, None))
                    .child(mode("api-mode-saved", "Saved", Mode::Collections, None))
                    .child(mode(
                        "api-mode-history",
                        "History",
                        Mode::History,
                        Some(history),
                    ))
                    .child(mode("api-mode-env", "Env", Mode::Environments, None)),
            )
            .into_any_element()
    }

    /// The active environment as a chip that opens the Env view.
    fn render_environment_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let name = self
            .library
            .read(cx)
            .active_environment()
            .map(|environment| environment.name.clone());
        let active = name.is_some();
        div()
            .id("api-environment-chip")
            .debug_selector(|| "api-environment-chip".into())
            .flex_none()
            .max_w(px(140.0))
            .h(px(22.0))
            .px(px(7.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(Radius::CHIP))
            .cursor_pointer()
            .bg(colors.primary.alpha(0.045))
            .hover(move |chip| chip.bg(colors.primary.alpha(0.08)))
            .child(div().size(px(6.0)).rounded_full().bg(if active {
                Ink::FRESH
            } else {
                colors.tertiary
            }))
            .child(
                div()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if active {
                        colors.primary
                    } else {
                        colors.secondary
                    })
                    .child(name.unwrap_or_else(|| "No environment".to_owned())),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.commit_field();
                this.mode = Mode::Environments;
                cx.notify();
            }))
            .into_any_element()
    }

    // MARK: Request builder

    fn render_request_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let method = self.draft.method;
        let sending = self.is_sending();
        let method_chip = div()
            .id("api-method")
            .debug_selector(|| "api-method".into())
            .flex_none()
            .h(px(FIELD_HEIGHT))
            .px(px(7.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .rounded(px(Radius::CHIP))
            .cursor_pointer()
            .bg(method_color(method, colors).alpha(0.12))
            .hover(move |chip| chip.bg(method_color(method, colors).alpha(0.18)))
            .font_family(mono())
            .text_size(px(10.5))
            .font_weight(FontWeight::BOLD)
            .text_color(method_color(method, colors))
            .child(method.name())
            .child(icon(
                IconName::ChevronDown,
                9.0,
                method_color(method, colors),
            ))
            .on_click(cx.listener(|this, _, _, cx| {
                this.commit_field();
                this.method_menu_open = !this.method_menu_open;
                cx.notify();
            }));
        let menu = self.method_menu_open.then(|| {
            let mut menu = div()
                .absolute()
                .top(px(FIELD_HEIGHT + 4.0))
                .left(px(0.0))
                .w(px(112.0))
                .p(px(4.0))
                .flex()
                .flex_col()
                .rounded(px(Radius::CARD))
                .bg(colors.floating_fill())
                .border_1()
                .border_color(colors.floating_stroke())
                .shadow_md();
            for each in Method::ALL {
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("api-method-{}", each.name())))
                        .debug_selector(move || format!("api-method-{}", each.name()))
                        .h(px(24.0))
                        .px(px(7.0))
                        .flex()
                        .items_center()
                        .rounded(px(Radius::CHIP))
                        .cursor_pointer()
                        .hover(move |row| row.bg(colors.primary.alpha(0.07)))
                        .when(each == method, |row| row.bg(colors.primary.alpha(0.06)))
                        .font_family(mono())
                        .text_size(px(10.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(method_color(each, colors))
                        .child(each.name())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_method(each, cx);
                            cx.stop_propagation();
                        })),
                );
            }
            deferred(menu.occlude()).with_priority(1)
        });
        let send = div()
            .id("api-send")
            .debug_selector(|| "api-send".into())
            .flex_none()
            .h(px(FIELD_HEIGHT))
            .px(px(11.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(Radius::CHIP))
            .cursor_pointer()
            .text_size(px(11.0))
            .font_weight(FontWeight::SEMIBOLD)
            .map(|button| {
                if sending {
                    button
                        .bg(colors.primary.alpha(0.08))
                        .hover(move |button| button.bg(colors.primary.alpha(0.12)))
                        .text_color(colors.primary)
                        .child("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
                } else {
                    button
                        .bg(ACCENT)
                        .hover(|button| button.bg(gpui::rgba(0x4f83f1ff)))
                        .text_color(gpui::rgba(0xffffffff))
                        .child("Send")
                        .on_click(cx.listener(|this, _, _, cx| this.send(cx)))
                }
            });
        div()
            .flex_none()
            .px(px(INSET))
            .pt(px(INSET))
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(method_chip)
                    .children(menu),
            )
            .child(
                self.render_field(Field::Url, "https://api.example.com/v1/items", false, cx)
                    .flex_1()
                    .font_family(mono())
                    .text_size(px(11.0)),
            )
            .child(send)
            .into_any_element()
    }

    /// Under the address: the one that goes out with variables filled in,
    /// or which variables are missing. A secret's value never shows here.
    fn render_resolved_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = self.colors;
        if self.draft.url.trim().is_empty() {
            return None;
        }
        let library = self.library.read(cx);
        let environment = library.active_environment();
        let resolved: Resolved = model::resolve(self.draft.url.trim(), environment);
        let uses_variables = self.draft.url.contains("{{");
        let missing: Vec<String> = self
            .draft
            .variable_names()
            .into_iter()
            .filter(|name| {
                environment
                    .and_then(|environment| environment.lookup(name))
                    .is_none()
            })
            .collect();
        if !uses_variables && missing.is_empty() {
            return None;
        }
        let text = if !missing.is_empty() {
            format!(
                "No value for {}",
                missing
                    .iter()
                    .map(|name| format!("{{{{{name}}}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else if resolved.uses_secret {
            "→ uses a secret variable".to_owned()
        } else {
            format!("→ {}", model::absolute_url(&resolved.text))
        };
        Some(
            div()
                .flex_none()
                .px(px(INSET + 2.0))
                .pt(px(5.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_family(mono())
                .text_size(px(10.0))
                .text_color(if missing.is_empty() {
                    colors.tertiary
                } else {
                    Ink::ATTENTION
                })
                .child(text)
                .into_any_element(),
        )
    }

    fn render_request_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let live = |rows: &[Pair]| rows.iter().filter(|pair| pair.is_live()).count();
        let counts = [
            (RequestTab::Params, "Params", live(&self.draft.params)),
            (RequestTab::Headers, "Headers", live(&self.draft.headers)),
            (RequestTab::Body, "Body", 0),
            (RequestTab::Auth, "Auth", 0),
        ];
        let mut track = details_ui::segmented_track(colors);
        for (tab, label, count) in counts {
            track = track.child(
                details_ui::segment(
                    SharedString::from(format!("api-request-tab-{label}")),
                    label,
                    (count > 0).then_some(count),
                    self.request_tab == tab,
                    colors,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.commit_field();
                    this.request_tab = tab;
                    cx.notify();
                })),
            );
        }
        let save_label = if self.is_saved(cx) { "Saved" } else { "Save" };
        div()
            .flex_none()
            .px(px(INSET))
            .pt(px(10.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(track)
            .child(
                text_button("api-save", save_label, colors)
                    .debug_selector(|| "api-save".into())
                    .on_click(cx.listener(|this, _, _, cx| this.save_to_collection(cx))),
            )
            .into_any_element()
    }

    /// Ely's `KeyValueInput`: a checkbox, a name and a value per row, a
    /// remove button on hover, and a row that adds one.
    fn render_pairs(
        &self,
        tab: RequestTab,
        rows: &[Pair],
        field: fn(usize, Column) -> Field,
        name_placeholder: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors;
        let mut list = div().flex().flex_col().gap(px(4.0));
        for (index, pair) in rows.iter().enumerate() {
            let enabled = pair.enabled;
            let group = SharedString::from(format!("api-row-{tab:?}-{index}"));
            list = list.child(
                div()
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("api-toggle-{tab:?}-{index}")))
                            .flex_none()
                            .size(px(14.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.5))
                            .cursor_pointer()
                            .border_1()
                            .border_color(if enabled {
                                ACCENT
                            } else {
                                colors.primary.alpha(0.2)
                            })
                            .when(enabled, |check| check.bg(ACCENT))
                            .when(enabled, |check| {
                                check.child(icon(IconName::Check, 8.0, gpui::rgba(0xffffffff)))
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_row(tab, index, cx)),
                            ),
                    )
                    .child(
                        self.render_field(field(index, Column::Name), name_placeholder, false, cx)
                            .flex_1()
                            .font_family(mono())
                            .when(!enabled, |field| field.opacity(0.5)),
                    )
                    .child(
                        self.render_field(field(index, Column::Value), "value", false, cx)
                            .flex_1()
                            .when(!enabled, |field| field.opacity(0.5)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("api-remove-{tab:?}-{index}")))
                            .flex_none()
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(Radius::CHIP))
                            .cursor_pointer()
                            .invisible()
                            .group_hover(group, |button| button.visible())
                            .hover(move |button| button.bg(colors.primary.alpha(0.07)))
                            .child(icon(IconName::Close, 9.0, colors.secondary))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_row(tab, index, cx)),
                            ),
                    ),
            );
        }
        list.child(
            div().flex().child(
                text_button(
                    SharedString::from(format!("api-add-{tab:?}")),
                    "Add",
                    colors,
                )
                .child(icon(IconName::Plus, 9.0, colors.secondary))
                .on_click(cx.listener(move |this, _, window, cx| this.add_row(tab, window, cx))),
            ),
        )
        .into_any_element()
    }

    fn render_body_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let kind = self.draft.body_kind;
        let mut track = details_ui::segmented_track(colors);
        for each in BodyKind::ALL {
            track = track.child(
                details_ui::segment(
                    SharedString::from(format!("api-body-{}", each.label())),
                    each.label(),
                    None,
                    kind == each,
                    colors,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_body_kind(each, cx))),
            );
        }
        let json_state =
            (kind == BodyKind::Json && !self.draft.body.trim().is_empty()).then(|| {
                serde_json::from_str::<serde::de::IgnoredAny>(&self.draft.body)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });
        let body =
            match kind {
                BodyKind::None => div()
                    .py(px(10.0))
                    .text_size(px(11.0))
                    .text_color(colors.tertiary)
                    .child("This request has no body.")
                    .into_any_element(),
                BodyKind::Form => {
                    self.render_pairs(RequestTab::Body, &self.draft.form, Field::Form, "field", cx)
                }
                BodyKind::Json | BodyKind::Raw => div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(
                        self.render_field(
                            Field::Body,
                            if kind == BodyKind::Json {
                                "{ \"name\": \"value\" }"
                            } else {
                                "Body text"
                            },
                            false,
                            cx,
                        )
                        .font_family(mono())
                        .text_size(px(11.0)),
                    )
                    .when_some(json_state, |column, state| {
                        column.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(px(10.5))
                                .child(match &state {
                                    Ok(()) => div().text_color(Ink::FRESH).child("Valid JSON"),
                                    Err(error) => div()
                                        .min_w(px(0.0))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_color(Ink::ATTENTION)
                                        .child(error.clone()),
                                })
                                .when(state.is_ok(), |row| {
                                    row.child(text_button("api-format", "Format", colors).on_click(
                                        cx.listener(|this, _, _, cx| this.format_body(cx)),
                                    ))
                                }),
                        )
                    })
                    .into_any_element(),
            };
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex().child(track))
            .child(body)
            .into_any_element()
    }

    fn render_auth(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let kinds = [
            ("None", Auth::None),
            (
                "Bearer",
                Auth::Bearer {
                    token: String::new(),
                },
            ),
            (
                "Basic",
                Auth::Basic {
                    user: String::new(),
                    password: String::new(),
                },
            ),
        ];
        let mut track = details_ui::segmented_track(colors);
        for (label, auth) in kinds {
            let active = std::mem::discriminant(&self.draft.auth) == std::mem::discriminant(&auth);
            track = track.child(
                details_ui::segment(
                    SharedString::from(format!("api-auth-{label}")),
                    label,
                    None,
                    active,
                    colors,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_auth_kind(auth.clone(), cx))),
            );
        }
        let labelled = |label: &'static str, field: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_none()
                        .w(px(64.0))
                        .text_size(px(11.0))
                        .text_color(colors.secondary)
                        .child(label),
                )
                .child(div().flex_1().min_w(px(0.0)).child(field))
        };
        let fields = match &self.draft.auth {
            Auth::None => div()
                .text_size(px(11.0))
                .text_color(colors.tertiary)
                .child("No sign-in. Headers you add still go out.")
                .into_any_element(),
            Auth::Bearer { .. } => labelled(
                "Token",
                self.render_field(
                    Field::BearerToken,
                    "{{token}}",
                    Field::BearerToken.masked(),
                    cx,
                )
                .font_family(mono())
                .into_any_element(),
            )
            .into_any_element(),
            Auth::Basic { .. } => div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(labelled(
                    "User",
                    self.render_field(Field::BasicUser, "user", false, cx)
                        .into_any_element(),
                ))
                .child(labelled(
                    "Password",
                    self.render_field(
                        Field::BasicPassword,
                        "password",
                        Field::BasicPassword.masked(),
                        cx,
                    )
                    .into_any_element(),
                ))
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(div().flex().child(track))
            .child(fields)
            .into_any_element()
    }

    fn render_request(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = match self.request_tab {
            RequestTab::Params => self.render_pairs(
                RequestTab::Params,
                &self.draft.params,
                Field::Param,
                "parameter",
                cx,
            ),
            RequestTab::Headers => self.render_pairs(
                RequestTab::Headers,
                &self.draft.headers,
                Field::Header,
                "Header",
                cx,
            ),
            RequestTab::Body => self.render_body_editor(cx),
            RequestTab::Auth => self.render_auth(cx),
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .child(self.render_request_bar(cx))
            .children(self.render_resolved_line(cx))
            .child(self.render_request_tabs(cx))
            .child(
                div()
                    .id("api-request-panel")
                    .max_h(px(260.0))
                    .overflow_y_scroll()
                    .px(px(INSET))
                    .pt(px(8.0))
                    .pb(px(10.0))
                    .child(panel),
            )
            .into_any_element()
    }

    // MARK: Response viewer

    fn render_response(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let header = div()
            .flex_none()
            .h(px(34.0))
            .px(px(INSET))
            .flex()
            .items_center()
            .gap(px(8.0))
            .border_t_1()
            .border_color(colors.primary.alpha(0.065));
        match &self.outcome {
            None if self.is_sending() => div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    header.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(colors.secondary)
                            .child("Sending…"),
                    ),
                )
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .border_t_1()
                .border_color(colors.primary.alpha(0.065))
                .text_color(colors.tertiary)
                .child(icon(IconName::Server, 20.0, colors.tertiary))
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.secondary)
                        .child("No response yet"),
                )
                .child(
                    div()
                        .max_w(px(240.0))
                        .text_center()
                        .text_size(px(11.0))
                        .line_height(px(16.0))
                        .child(
                            "Send the request to see its status, headers and body here. ⌘↩ sends.",
                        ),
                )
                .into_any_element(),
            Some(Outcome::Failed(message)) => div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    header
                        .child(icon(IconName::Warning, 11.0, Ink::ATTENTION))
                        .child(
                            div()
                                .min_w(px(0.0))
                                .text_size(px(11.5))
                                .text_color(colors.primary)
                                .child(message.clone()),
                        )
                        .when(self.is_sending(), |header| {
                            header.child(
                                div()
                                    .ml_auto()
                                    .text_size(px(10.5))
                                    .text_color(colors.tertiary)
                                    .child("Sending…"),
                            )
                        }),
                )
                .into_any_element(),
            Some(Outcome::Response(view)) => self.render_response_view(view, header, cx),
        }
    }

    fn render_response_view(
        &self,
        view: &ResponseView,
        header: Div,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors;
        let response = &view.response;
        let reason = if response.reason.is_empty() {
            super::http::reason(response.status).to_owned()
        } else {
            response.reason.clone()
        };
        let tone = status_color(response.status, colors);
        let quiet = |text: String| {
            div()
                .flex_none()
                .text_size(px(10.5))
                .text_color(colors.tertiary)
                .child(text)
        };
        let mut tabs = details_ui::segmented_track(colors).flex_none().w(px(150.0));
        for (tab, label, count) in [
            (ResponseTab::Body, "Body", None),
            (
                ResponseTab::Headers,
                "Headers",
                Some(response.headers.len()),
            ),
        ] {
            tabs = tabs.child(
                details_ui::segment(
                    SharedString::from(format!("api-response-tab-{label}")),
                    label,
                    count,
                    self.response_tab == tab,
                    colors,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.response_tab = tab;
                    cx.notify();
                })),
            );
        }
        let status = div()
            .flex_none()
            .h(px(20.0))
            .px(px(7.0))
            .flex()
            .items_center()
            .rounded(px(Radius::CHIP))
            .bg(tone.alpha(0.14))
            .font_family(mono())
            .text_size(px(10.5))
            .font_weight(FontWeight::BOLD)
            .text_color(tone)
            .child(format!("{} {}", response.status, reason).trim().to_owned());
        let header = header
            .child(status)
            .child(quiet(super::http::format_duration(response.took_ms)))
            .child(quiet(super::http::format_size(response.size)))
            .when(response.redirects > 0, |header| {
                header.child(quiet(format!(
                    "{} redirect{}",
                    response.redirects,
                    if response.redirects == 1 { "" } else { "s" }
                )))
            })
            .child(div().flex_1())
            .child(tabs);
        let body = match self.response_tab {
            ResponseTab::Headers => self.render_response_headers(response, cx),
            ResponseTab::Body => self.render_body_view(view, cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .when(response.truncated, |column| {
                column.child(
                    div()
                        .flex_none()
                        .px(px(INSET))
                        .pb(px(4.0))
                        .text_size(px(10.5))
                        .text_color(Ink::ATTENTION)
                        .child(format!(
                            "Showing the first {} of the body.",
                            super::http::format_size(response.body.len() as u64)
                        )),
                )
            })
            .child(div().min_h(px(0.0)).flex_1().child(body))
            .into_any_element()
    }

    /// Ely's `HeadersTable`: names in a quiet mono column, values beside.
    fn render_response_headers(
        &self,
        response: &super::http::ApiResponse,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors;
        let mut table = div().flex().flex_col();
        for (index, (name, value)) in response.headers.iter().enumerate() {
            table = table
                .when(index > 0, |table| table.child(details_ui::hairline(colors)))
                .child(
                    div()
                        .flex()
                        .gap(px(10.0))
                        .py(px(5.0))
                        .text_size(px(11.0))
                        .child(
                            div()
                                .flex_none()
                                .w(relative(0.38))
                                .overflow_hidden()
                                .text_ellipsis()
                                .font_family(mono())
                                .text_color(colors.secondary)
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex_1()
                                .font_family(mono())
                                .text_color(colors.primary)
                                .child(value.clone()),
                        ),
                );
        }
        div()
            .id("api-response-headers")
            .size_full()
            .overflow_y_scroll()
            .px(px(INSET))
            .pb(px(INSET))
            .child(table)
            .into_any_element()
    }

    /// The body: pretty JSON with fold chevrons, or the raw lines. Both are
    /// virtualized; a body can be megabytes.
    fn render_body_view(&self, view: &ResponseView, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let pretty = view.pretty.clone().filter(|_| !self.response_raw);
        let has_pretty = view.pretty.is_some();
        let raw = self.response_raw;
        let toolbar = div()
            .flex_none()
            .px(px(INSET - 4.0))
            .pb(px(2.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .when(has_pretty, |bar| {
                bar.child(
                    text_button(
                        "api-body-pretty",
                        if raw { "Pretty" } else { "Raw" },
                        colors,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.response_raw = !this.response_raw;
                        cx.notify();
                    })),
                )
                .when(!raw, |bar| {
                    bar.child(
                        text_button("api-body-collapse", "Collapse", colors)
                            .on_click(cx.listener(|this, _, _, cx| this.fold_all(true, cx))),
                    )
                    .child(
                        text_button("api-body-expand", "Expand", colors)
                            .on_click(cx.listener(|this, _, _, cx| this.fold_all(false, cx))),
                    )
                })
            })
            .child(div().flex_1())
            .child(
                text_button("api-body-copy", "Copy", colors)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_response(cx))),
            );
        let list = match pretty {
            Some(pretty) => {
                let rows = Arc::new(pretty.visible(&self.folded));
                let folded = self.folded.clone();
                let entity = cx.entity();
                uniform_list("api-response-json", rows.len(), move |range, _, _| {
                    range
                        .map(|row| json_row(&pretty, rows[row], &folded, colors, entity.clone()))
                        .collect()
                })
                .track_scroll(&self.body_scroll)
                .size_full()
                .into_any_element()
            }
            None if view.raw_lines.is_empty() => div()
                .px(px(INSET))
                .text_size(px(11.0))
                .text_color(colors.tertiary)
                .child("Empty body")
                .into_any_element(),
            None => {
                let lines = view.raw_lines.clone();
                uniform_list("api-response-raw", lines.len(), move |range, _, _| {
                    range
                        .map(|index| raw_row(index, &lines[index], colors))
                        .collect()
                })
                .track_scroll(&self.body_scroll)
                .size_full()
                .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().min_h(px(0.0)).flex_1().child(list))
            .into_any_element()
    }

    // MARK: Collections

    fn render_collections(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let library = self.library.read(cx);
        let rows = model::tree_rows(&library.project.collections, &self.tree_open);
        let editing_folder = self.focused_field() == Some(Field::FolderName);
        let mut list = div().flex().flex_col().gap(px(1.0));
        if rows.is_empty() {
            list = list.child(empty_note(
                colors,
                "No saved requests",
                "Save the request you are editing with Save or ⌘S; it lands in the selected folder.",
            ));
        }
        for row in rows {
            list = list.child(self.render_tree_row(&row, editing_folder, cx));
        }
        div()
            .id("api-collections")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(INSET - 4.0))
                    .py(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        text_button("api-new-folder", "New folder", colors)
                            .child(icon(IconName::Folder, 10.0, colors.secondary))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_folder(window, cx)),
                            ),
                    )
                    .child(
                        text_button("api-save-current", "Save current request", colors)
                            .on_click(cx.listener(|this, _, _, cx| this.save_to_collection(cx))),
                    ),
            )
            .child(div().px(px(INSET - 4.0)).child(list))
            .into_any_element()
    }

    fn render_tree_row(
        &self,
        row: &TreeRow,
        editing_folder: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors;
        let selected = self.tree_selected.as_deref() == Some(row.id.as_str())
            || (row.method.is_some() && row.id == self.draft.id);
        let id = row.id.clone();
        let remove = row.id.clone();
        let group = SharedString::from(format!("api-tree-{}", row.id));
        let leading = match row.method {
            Some(method) => method_tag(method, colors),
            None => div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(icon(
                    if row.open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    9.0,
                    colors.tertiary,
                ))
                .child(icon(IconName::Folder, 11.0, colors.secondary))
                .into_any_element(),
        };
        let label: AnyElement = if row.method.is_none() && selected && editing_folder {
            self.render_field(Field::FolderName, "Folder name", false, cx)
                .flex_1()
                .into_any_element()
        } else {
            div()
                .min_w(px(0.0))
                .flex_1()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(row.label.clone())
                .into_any_element()
        };
        let is_folder = row.method.is_none();
        div()
            .id(SharedString::from(format!("api-tree-row-{}", row.id)))
            .group(group.clone())
            .h(px(26.0))
            .pl(px(6.0 + row.depth as f32 * 14.0))
            .pr(px(4.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(Radius::ROW))
            .cursor_pointer()
            .text_size(px(Typo::ROW.size - 1.5))
            .text_color(colors.primary)
            .when(selected, |row| row.bg(colors.primary.alpha(0.08)))
            .hover(move |row| row.bg(colors.primary.alpha(0.05)))
            .child(leading)
            .child(label)
            .when(is_folder && row.count > 0, |line| {
                line.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(colors.tertiary)
                        .child(row.count.to_string()),
                )
            })
            .when(is_folder && selected && !editing_folder, |line| {
                line.child(
                    text_button(
                        SharedString::from(format!("api-rename-{}", row.id)),
                        "Rename",
                        colors,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_field(Field::FolderName, window, cx);
                        cx.stop_propagation();
                    })),
                )
            })
            .child(
                div()
                    .id(SharedString::from(format!("api-tree-delete-{}", row.id)))
                    .flex_none()
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(Radius::CHIP))
                    .invisible()
                    .group_hover(group, |button| button.visible())
                    .hover(move |button| button.bg(colors.primary.alpha(0.08)))
                    .child(icon(IconName::Trash, 10.0, colors.secondary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_tree_item(&remove, cx);
                        cx.stop_propagation();
                    })),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.activate_tree_row(&id, cx)))
            .into_any_element()
    }

    // MARK: History

    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let history = self.library.read(cx).project.history.clone();
        let mut list = div().flex().flex_col().gap(px(1.0));
        if history.is_empty() {
            list = list.child(empty_note(
                colors,
                "Nothing sent yet",
                "Every request you send lands here, newest first.",
            ));
        }
        for (index, entry) in history.iter().enumerate() {
            let tone = entry
                .status
                .map_or(Ink::DANGER, |status| status_color(status, colors));
            list = list.child(
                div()
                    .id(SharedString::from(format!("api-history-{index}")))
                    .h(px(28.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(Radius::ROW))
                    .cursor_pointer()
                    .hover(move |row| row.bg(colors.primary.alpha(0.05)))
                    .child(method_tag(entry.request.method, colors))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(mono())
                            .text_size(px(10.5))
                            .text_color(colors.primary)
                            .child(entry.request.url.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(mono())
                            .text_size(px(10.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(tone)
                            .child(
                                entry
                                    .status
                                    .map_or("ERR".to_owned(), |status| status.to_string()),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(44.0))
                            .text_right()
                            .text_size(px(10.0))
                            .text_color(colors.tertiary)
                            .child(details_ui::relative_time(entry.at_ms as f64)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_history(index, cx))),
            );
        }
        div()
            .id("api-history")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(INSET - 4.0))
                    .py(px(6.0))
                    .flex()
                    .items_center()
                    .child(div().flex_1())
                    .when(!history.is_empty(), |bar| {
                        bar.child(
                            text_button("api-clear-history", "Clear", colors)
                                .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx))),
                        )
                    }),
            )
            .child(div().px(px(INSET - 4.0)).child(list))
            .into_any_element()
    }

    // MARK: Environments

    /// Ely's `EnvironmentSelector`: pick the active environment, then edit
    /// its variables; a secret shows as dots until its lock is opened.
    fn render_environments(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors;
        let library = self.library.read(cx);
        let environments = library.project.environments.clone();
        let active = library.project.active_environment.clone();
        let mut picker = div().flex().flex_col().gap(px(1.0));
        picker = picker.child(self.render_environment_option(
            None,
            "No environment",
            active.is_none(),
            cx,
        ));
        for environment in &environments {
            picker = picker.child(self.render_environment_option(
                Some(environment.id.clone()),
                &environment.name,
                active.as_deref() == Some(environment.id.as_str()),
                cx,
            ));
        }
        let editor = self
            .editing_environment
            .as_ref()
            .and_then(|id| {
                environments
                    .iter()
                    .find(|environment| &environment.id == id)
            })
            .map(|environment| self.render_variables(environment, cx));
        div()
            .id("api-environments")
            .size_full()
            .overflow_y_scroll()
            .px(px(INSET))
            .py(px(8.0))
            .flex()
            .flex_col()
            .gap(px(details_ui::SECTION_GAP - 4.0))
            .child(details_ui::section(
                "Environment",
                Some(
                    text_button("api-new-environment", "New", colors)
                        .child(icon(IconName::Plus, 9.0, colors.secondary))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.new_environment(window, cx)),
                        )
                        .into_any_element(),
                ),
                details_ui::card(colors).p(px(3.0)).child(picker),
                colors,
            ))
            .children(editor)
            .into_any_element()
    }

    fn render_environment_option(
        &self,
        id: Option<String>,
        name: &str,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors;
        let element_id = SharedString::from(format!(
            "api-environment-{}",
            id.as_deref().unwrap_or("none")
        ));
        let delete = id.clone();
        let group = element_id.clone();
        div()
            .id(element_id)
            .group(group.clone())
            .h(px(26.0))
            .px(px(7.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(Radius::ROW))
            .cursor_pointer()
            .hover(move |row| row.bg(colors.primary.alpha(0.05)))
            .child(
                div()
                    .flex_none()
                    .size(px(12.0))
                    .rounded_full()
                    .border_1()
                    .border_color(if active {
                        ACCENT
                    } else {
                        colors.primary.alpha(0.25)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(active, |dot| {
                        dot.child(div().size(px(6.0)).rounded_full().bg(ACCENT))
                    }),
            )
            .child(
                div()
                    .min_w(px(0.0))
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(11.5))
                    .text_color(if id.is_some() {
                        colors.primary
                    } else {
                        colors.secondary
                    })
                    .child(name.to_owned()),
            )
            .when_some(delete, |row, delete| {
                row.child(
                    div()
                        .id(SharedString::from(format!(
                            "api-environment-delete-{delete}"
                        )))
                        .flex_none()
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(Radius::CHIP))
                        .invisible()
                        .group_hover(group, |button| button.visible())
                        .hover(move |button| button.bg(colors.primary.alpha(0.08)))
                        .child(icon(IconName::Trash, 10.0, colors.secondary))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.delete_environment(&delete, cx);
                            cx.stop_propagation();
                        })),
                )
            })
            .on_click(
                cx.listener(move |this, _, _, cx| this.set_active_environment(id.clone(), cx)),
            )
            .into_any_element()
    }

    fn render_variables(
        &self,
        environment: &model::Environment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors;
        let mut rows = div().flex().flex_col().gap(px(4.0));
        for (index, variable) in environment.variables.iter().enumerate() {
            let secret = variable.secret;
            let revealed = self.revealed.contains(&(environment.id.clone(), index));
            let group = SharedString::from(format!("api-variable-{index}"));
            rows = rows.child(
                div()
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(
                        self.render_field(Field::Variable(index, Column::Name), "name", false, cx)
                            .flex_1()
                            .font_family(mono()),
                    )
                    .child(
                        self.render_field(
                            Field::Variable(index, Column::Value),
                            "value",
                            secret && !revealed,
                            cx,
                        )
                        .flex_1(),
                    )
                    .when(secret, |row| {
                        row.child(
                            text_button(
                                SharedString::from(format!("api-variable-reveal-{index}")),
                                if revealed { "Hide" } else { "Show" },
                                colors,
                            )
                            .debug_selector(move || format!("api-variable-reveal-{index}"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_reveal(index, cx)),
                            ),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("api-variable-secret-{index}")))
                            .debug_selector(move || format!("api-variable-secret-{index}"))
                            .flex_none()
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(Radius::CHIP))
                            .cursor_pointer()
                            .when(!secret, |button| {
                                button
                                    .invisible()
                                    .group_hover(group.clone(), |button| button.visible())
                            })
                            .hover(move |button| button.bg(colors.primary.alpha(0.08)))
                            .child(icon(
                                IconName::Lock,
                                10.0,
                                if secret {
                                    Ink::ATTENTION
                                } else {
                                    colors.tertiary
                                },
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.update_variable(
                                    index,
                                    |variables| {
                                        if let Some(variable) = variables.get_mut(index) {
                                            variable.secret = !variable.secret;
                                        }
                                    },
                                    cx,
                                )
                            })),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("api-variable-remove-{index}")))
                            .flex_none()
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(Radius::CHIP))
                            .cursor_pointer()
                            .invisible()
                            .group_hover(group, |button| button.visible())
                            .hover(move |button| button.bg(colors.primary.alpha(0.08)))
                            .child(icon(IconName::Close, 9.0, colors.secondary))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.update_variable(
                                    index,
                                    |variables| {
                                        if index < variables.len() {
                                            variables.remove(index);
                                        }
                                    },
                                    cx,
                                )
                            })),
                    ),
            );
        }
        rows = rows.child(
            div().flex().child(
                text_button("api-add-variable", "Add variable", colors)
                    .child(icon(IconName::Plus, 9.0, colors.secondary))
                    .on_click(cx.listener(|this, _, window, cx| this.add_variable(window, cx))),
            ),
        );
        details_ui::section(
            "Variables",
            Some(details_ui::count_label(environment.variables.len(), colors)),
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_none()
                                .w(px(40.0))
                                .text_size(px(11.0))
                                .text_color(colors.secondary)
                                .child("Name"),
                        )
                        .child(self.render_field(Field::EnvName, "Environment name", false, cx).flex_1()),
                )
                .child(rows)
                .child(
                    div()
                        .text_size(px(10.5))
                        .line_height(px(15.0))
                        .text_color(colors.tertiary)
                        .child("Use a variable as {{name}} in the URL, headers, body or auth. Lock marks it secret: it shows as dots and never in the URL preview."),
                ),
            colors,
        )
        .into_any_element()
    }
}

fn empty_note(colors: SemanticColors, title: &'static str, detail: &'static str) -> AnyElement {
    div()
        .py(px(24.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(5.0))
        .text_center()
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.secondary)
                .child(title),
        )
        .child(
            div()
                .max_w(px(240.0))
                .text_size(px(11.0))
                .line_height(px(16.0))
                .text_color(colors.tertiary)
                .child(detail),
        )
        .into_any_element()
}

fn json_row(
    pretty: &PrettyJson,
    index: usize,
    folded: &HashSet<usize>,
    colors: SemanticColors,
    entity: gpui::Entity<ApiClient>,
) -> AnyElement {
    let line = &pretty.lines[index];
    let foldable = line.fold_end.is_some();
    let is_folded = foldable && folded.contains(&index);
    let mut text = line.text.clone();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = line
        .spans
        .iter()
        .map(|(range, token)| {
            (
                range.clone(),
                HighlightStyle {
                    color: Some(token_color(*token, colors).into()),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect();
    if is_folded {
        let start = text.len();
        text.push_str(" … ");
        highlights.push((
            start..text.len(),
            HighlightStyle {
                color: Some(colors.tertiary.into()),
                ..HighlightStyle::default()
            },
        ));
        if let Some(tail) = pretty.folded_tail(index) {
            let start = text.len();
            text.push_str(&tail);
            highlights.push((
                start..text.len(),
                HighlightStyle {
                    color: Some(colors.tertiary.into()),
                    ..HighlightStyle::default()
                },
            ));
        }
    }
    div()
        .id(("api-json-line", index))
        .h(px(ROW_HEIGHT))
        .px(px(INSET - 4.0))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .font_family(mono())
        .text_size(px(11.0))
        .child(
            div()
                .flex_none()
                .w(px(36.0))
                .pr(px(8.0))
                .text_right()
                .text_size(px(10.0))
                .text_color(colors.tertiary.alpha(0.6))
                .child((index + 1).to_string()),
        )
        .child(
            div()
                .flex_none()
                .w(px(12.0))
                .flex()
                .items_center()
                .when(foldable, |chevron| {
                    chevron.child(icon(
                        if is_folded {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        },
                        8.0,
                        colors.tertiary,
                    ))
                }),
        )
        .child(StyledText::new(text).with_highlights(highlights))
        .when(foldable, |row| {
            row.cursor_pointer()
                .hover(move |row| row.bg(colors.primary.alpha(0.04)))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| this.toggle_fold(index, cx));
                })
        })
        .into_any_element()
}

fn raw_row(index: usize, line: &str, colors: SemanticColors) -> AnyElement {
    // A minified megabyte arrives as one line; draw a screenful of it.
    const MAX_CHARS: usize = 2000;
    let shown = match line.char_indices().nth(MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    };
    div()
        .h(px(ROW_HEIGHT))
        .px(px(INSET - 4.0))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .font_family(mono())
        .text_size(px(11.0))
        .text_color(colors.primary)
        .child(
            div()
                .flex_none()
                .w(px(36.0))
                .pr(px(8.0))
                .text_right()
                .text_size(px(10.0))
                .text_color(colors.tertiary.alpha(0.6))
                .child((index + 1).to_string()),
        )
        .child(shown)
        .into_any_element()
}

impl Render for ApiClient {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.colors;
        let body = match self.mode {
            Mode::Request => div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.render_request(cx))
                .child(
                    div()
                        .min_h(px(0.0))
                        .flex_1()
                        .child(self.render_response(cx)),
                )
                .into_any_element(),
            Mode::Collections => self.render_collections(cx),
            Mode::History => self.render_history(cx),
            Mode::Environments => self.render_environments(cx),
        };
        let error = self.library.read(cx).error.clone();
        div()
            .id("api-client")
            .key_context("ApiClient")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.field.is_some() || this.method_menu_open {
                        this.field = None;
                        this.method_menu_open = false;
                        cx.notify();
                    }
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_color(colors.primary)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(div().flex_1().min_w(px(0.0)).child(self.render_modes(cx)))
                    .child(
                        div()
                            .flex_none()
                            .h(px(40.0))
                            .pr(px(INSET))
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(colors.primary.alpha(0.065))
                            .child(self.render_environment_chip(cx)),
                    ),
            )
            .when_some(error, |surface, error| {
                surface.child(
                    div()
                        .flex_none()
                        .px(px(INSET))
                        .py(px(4.0))
                        .text_size(px(10.5))
                        .text_color(Ink::ATTENTION)
                        .child(error),
                )
            })
            .child(div().min_h(px(0.0)).flex_1().child(body))
    }
}
