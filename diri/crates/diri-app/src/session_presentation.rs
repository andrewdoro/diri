//! The working mark loops through pre-rasterized frames that the window
//! advances on its own (`diri_ui::FrameLoop`). No task, entity invalidation,
//! or frame request belongs to this mark, and no caller re-renders for it.

use diri_proto::{
    AgentKind as ProtoAgentKind, AttentionLevel as ProtoAttentionLevel, SessionRecord,
};
use diri_ui::{AgentKind, FrameLoop, Icon, IconName, Ink, SemanticColors, StatusState};
use gpui::{AnyElement, IntoElement, Role, div, prelude::*, px};
use std::time::Duration;

static FRAMES: [&str; 8] = [
    "icons/working-0.svg",
    "icons/working-1.svg",
    "icons/working-2.svg",
    "icons/working-3.svg",
    "icons/working-4.svg",
    "icons/working-5.svg",
    "icons/working-6.svg",
    "icons/working-7.svg",
];

/// One working-mark frame lasts this long: eight frames, one lap a second.
const FRAME_INTERVAL: Duration = Duration::from_millis(125);

pub(crate) fn activity_mark(state: StatusState, colors: SemanticColors) -> AnyElement {
    // Match the project badge column; the mark itself stays optically smaller.
    let slot = div()
        .size(px(18.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    match state {
        StatusState::Working => slot
            .child(FrameLoop::new(
                &FRAMES,
                FRAME_INTERVAL,
                14.0,
                colors.primary,
            ))
            .into_any_element(),
        StatusState::NeedsInput { destructive } => slot
            .child(Icon::new(
                IconName::Warning,
                14.0,
                Ink::on_surface(
                    if destructive {
                        Ink::DANGER
                    } else {
                        Ink::ATTENTION
                    },
                    colors,
                ),
            ))
            .into_any_element(),
        StatusState::DoneUnseen => slot
            .child(Icon::new(
                IconName::Check,
                14.0,
                Ink::on_surface(Ink::FRESH, colors),
            ))
            .into_any_element(),
        StatusState::Hibernated => slot
            .id("sleeping-status")
            .role(Role::Image)
            .aria_label(crate::i18n::t("session.sleeping"))
            .child(Icon::new(IconName::Moon, 13.0, colors.tertiary))
            .into_any_element(),
        StatusState::IdleSeen | StatusState::None => slot.into_any_element(),
    }
}

pub(crate) fn status_state(session: &SessionRecord, migrating: bool) -> StatusState {
    if migrating {
        return StatusState::Working;
    }
    if session.hibernation.is_some() {
        return StatusState::Hibernated;
    }
    match session.attention() {
        ProtoAttentionLevel::NeedsInput => StatusState::NeedsInput {
            destructive: session
                .needs_input
                .as_ref()
                .is_some_and(|detail| detail.risk_hint == diri_proto::RiskHint::Destructive),
        },
        ProtoAttentionLevel::DoneUnseen => StatusState::DoneUnseen,
        ProtoAttentionLevel::Working => StatusState::Working,
        ProtoAttentionLevel::IdleSeen => StatusState::IdleSeen,
        ProtoAttentionLevel::None | ProtoAttentionLevel::Unknown => StatusState::None,
    }
}

pub(crate) fn is_loading(session: &SessionRecord, migrating: bool) -> bool {
    !migrating
        && session.hibernation.is_none()
        && session.effective_kind() != &ProtoAgentKind::SHELL
        && matches!(session.status, diri_proto::SessionStatus::Starting)
}

pub(crate) fn ui_agent_kind(kind: &ProtoAgentKind) -> AgentKind {
    // Brand vocabulary, not a protocol type: a manifest agent the client has
    // no hand-drawn mark for falls back to the generic terminal treatment.
    match kind.id() {
        ProtoAgentKind::CLAUDE_CODE_ID => AgentKind::ClaudeCode,
        ProtoAgentKind::CODEX_ID => AgentKind::Codex,
        ProtoAgentKind::CURSOR_ID => AgentKind::Cursor,
        ProtoAgentKind::GEMINI_ID => AgentKind::Gemini,
        ProtoAgentKind::SHELL_ID => AgentKind::Shell,
        ProtoAgentKind::NOTE_ID => AgentKind::Note,
        _ => AgentKind::Generic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terminal_has_no_loading_or_working_indicator() {
        use crate::sidebar::{PreviewScenario, SidebarPreviewFixture};
        use diri_proto::SessionStatus;

        let mut session = SidebarPreviewFixture::make(PreviewScenario::Typical)
            .list
            .sessions
            .into_iter()
            .find(|session| session.kind == ProtoAgentKind::SHELL)
            .expect("shell fixture");
        for status in [
            SessionStatus::Starting,
            SessionStatus::Working,
            SessionStatus::Idle,
        ] {
            session.status = status;
            assert!(!is_loading(&session, false));
            assert_eq!(status_state(&session, false), StatusState::None);
        }

        session.foreground_agent = Some(ProtoAgentKind::CLAUDE_CODE);
        session.status = SessionStatus::Starting;
        assert!(is_loading(&session, false));
        assert_eq!(status_state(&session, false), StatusState::Working);
        assert!(!is_loading(&session, true));
    }

    #[test]
    fn every_activity_frame_is_embedded_in_the_app() {
        use gpui::AssetSource;
        for path in FRAMES {
            let bytes = diri_ui::IconAssets
                .load(path)
                .expect("load activity frame")
                .expect(path);
            let svg = std::str::from_utf8(bytes.as_ref()).expect(path);
            assert!(svg.starts_with("<svg"), "{path}");
            assert!(
                svg.contains("currentColor"),
                "{path} must follow the theme color"
            );
        }
    }
}
