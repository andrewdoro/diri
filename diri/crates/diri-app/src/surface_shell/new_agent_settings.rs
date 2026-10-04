//! Settings > General > New agents start in: per project, whether ⌘T and
//! New Agent open in the current checkout (the default, and what every
//! project did before) or in a fresh Diri worktree from the remote default
//! branch, fetched first so a stale local `main` is never the base.
use super::*;
use crate::store::NewAgentStart;

impl UtilitySurfaces {
    pub(super) fn new_agent_start_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.settings_colors();
        let mut projects: Vec<diri_proto::Project> = {
            let store = self.store.read().expect("session store lock poisoned");
            store
                .projects()
                .values()
                .filter(|project| project.host.is_none())
                .cloned()
                .collect()
        };
        if projects.is_empty() {
            return div().into_any_element();
        }
        projects.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.root.cmp(&right.root))
        });
        let mut rows = div().flex().flex_col();
        for (index, project) in projects.into_iter().enumerate() {
            if index > 0 {
                rows = rows.child(setting_divider(colors));
            }
            let start = self.prefs.new_agent_start(&project.id);
            let control = div()
                .flex_none()
                .h(px(24.0))
                .p(px(2.0))
                .flex()
                .items_center()
                .gap(px(1.0))
                .rounded(px(Radius::BADGE))
                .bg(colors.primary.alpha(0.04))
                .child(start_option(
                    &project.id,
                    NewAgentStart::CurrentCheckout,
                    "Current checkout",
                    start,
                    colors,
                    cx,
                ))
                .child(start_option(
                    &project.id,
                    NewAgentStart::FreshWorktree,
                    "Fresh worktree",
                    start,
                    colors,
                    cx,
                ));
            let detail = match start {
                NewAgentStart::CurrentCheckout => project.root.clone(),
                NewAgentStart::FreshWorktree => {
                    "New branch from the latest origin default branch".to_owned()
                }
            };
            rows = rows.child(setting_row(project.name.clone(), detail, control, colors));
        }
        setting_section("New agents start in", rows, colors).into_any_element()
    }
}

fn start_option(
    project: &diri_proto::ProjectId,
    option: NewAgentStart,
    label: &'static str,
    current: NewAgentStart,
    colors: SemanticColors,
    cx: &mut Context<UtilitySurfaces>,
) -> impl IntoElement {
    let selected = option == current;
    let id = SharedString::from(format!("new-agent-start-{}-{label}", project.0));
    let project = project.clone();
    div()
        .id(id)
        .h(px(20.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(Radius::BADGE - 1.0))
        .text_size(px(Typo::META.size))
        .text_color(if selected {
            colors.primary
        } else {
            colors.tertiary
        })
        .when(selected, |option| option.bg(colors.primary.alpha(0.10)))
        .when(!selected, |option| {
            option
                .cursor_pointer()
                .hover(move |option| option.text_color(colors.secondary))
        })
        .child(label)
        .on_click(cx.listener(move |this, _, _, cx| {
            let project = project.clone();
            this.update_prefs(move |prefs| prefs.set_new_agent_start(&project, option));
            cx.notify();
        }))
}
