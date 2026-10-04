// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! The inspector's pull request card and its review conversation, ported
//! from Ely's `git::review::{PullRequestCard, ReviewComment}`.
//!
//! The card is a pure projection of a [`PullRequestStatus`]. Everything it
//! does to the inspector (quote a block, ask about evidence, fold a section)
//! arrives as callbacks in [`PrCardActions`], so the inspector keeps
//! ownership of selection and Ask state.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use diri_proto::{PrCheck, PrDiscussionItem, PullRequestStatus, SessionId};
use diri_ui::{IconName, Ink, Radius, SemanticColors, Typo};
use gpui::{
    AnyElement, App, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px, rgba,
};

use crate::details_ui::{
    self, MERGED, avatar, avatar_group, diff_stat, ghost_button, hairline, icon, icon_tag, ref_tag,
    relative_time, selection_fill, selection_hover, selection_stroke, tag,
};
use crate::markdown::MarkdownDocument;
use crate::markdown_view::render_markdown;
use crate::quote::QuoteSource;
use crate::review_prompt::ReviewEvidence;

/// The newest conversation entries a folded card shows.
pub const DISCUSSION_PREVIEW: usize = 4;

/// Ask's warm accent, shared with the rest of the inspector.
const ASK_TINT: gpui::Rgba = gpui::Rgba {
    r: 0.914,
    g: 0.639,
    b: 0.506,
    a: 1.0,
};

/// Where a pull request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PullState {
    pub fn of(pull_request: &PullRequestStatus) -> Self {
        match pull_request.state.as_str() {
            "MERGED" => Self::Merged,
            "CLOSED" => Self::Closed,
            _ if pull_request.is_draft => Self::Draft,
            _ => Self::Open,
        }
    }

    pub fn look(self, colors: SemanticColors) -> (IconName, gpui::Rgba, &'static str) {
        match self {
            Self::Open => (IconName::PullRequest, Ink::FRESH, "Open"),
            Self::Draft => (IconName::PullRequest, colors.secondary, "Draft"),
            Self::Merged => (IconName::Merge, MERGED, "Merged"),
            Self::Closed => (IconName::PullRequest, Ink::DANGER, "Closed"),
        }
    }
}

/// A reviewer's verdict, from GitHub's review `state`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
    Other(String),
}

impl ReviewVerdict {
    pub fn from_state(state: Option<&str>) -> Option<Self> {
        Some(match state? {
            "APPROVED" => Self::Approved,
            "CHANGES_REQUESTED" => Self::ChangesRequested,
            "COMMENTED" => Self::Commented,
            "DISMISSED" => Self::Dismissed,
            "PENDING" => Self::Pending,
            other => Self::Other(humanize_github_state(other)),
        })
    }

    pub fn label(&self) -> String {
        match self {
            Self::Approved => "Approved".to_owned(),
            Self::ChangesRequested => "Changes requested".to_owned(),
            Self::Commented => "Reviewed".to_owned(),
            Self::Dismissed => "Dismissed".to_owned(),
            Self::Pending => "Pending".to_owned(),
            Self::Other(label) => label.clone(),
        }
    }

    /// The verb phrase of a timeline event without a body.
    pub fn event_phrase(&self) -> String {
        match self {
            Self::Approved => "approved these changes".to_owned(),
            Self::ChangesRequested => "requested changes".to_owned(),
            Self::Commented => "reviewed".to_owned(),
            Self::Dismissed => "had a review dismissed".to_owned(),
            Self::Pending => "started a review".to_owned(),
            Self::Other(label) => label.to_ascii_lowercase(),
        }
    }

    /// Whether this verdict outranks an earlier one by the same reviewer.
    fn decisive(&self) -> bool {
        matches!(
            self,
            Self::Approved | Self::ChangesRequested | Self::Dismissed
        )
    }

    pub fn tone(&self, colors: SemanticColors) -> gpui::Rgba {
        match self {
            Self::Approved => Ink::FRESH,
            Self::ChangesRequested => Ink::DANGER,
            Self::Pending => Ink::ATTENTION,
            _ => colors.secondary,
        }
    }

    fn icon(&self) -> IconName {
        match self {
            Self::Approved => IconName::CheckCircle,
            Self::ChangesRequested => IconName::CloseCircle,
            _ => IconName::Comment,
        }
    }
}

/// The repository's overall review requirement, when GitHub reports one.
pub fn review_decision(pull_request: &PullRequestStatus) -> Option<(&'static str, gpui::Rgba)> {
    match pull_request.review_decision.as_deref()? {
        "APPROVED" => Some(("Approved", Ink::FRESH)),
        "CHANGES_REQUESTED" => Some(("Changes requested", Ink::DANGER)),
        "REVIEW_REQUIRED" => Some(("Review required", Ink::ATTENTION)),
        _ => None,
    }
}

/// Each reviewer once, in order of first review, with their standing
/// verdict: a later approval or change request replaces an earlier one, a
/// later plain comment does not.
pub fn reviewers(items: &[PrDiscussionItem]) -> Vec<(String, ReviewVerdict)> {
    let mut reviewers: Vec<(String, ReviewVerdict)> = Vec::new();
    for item in items.iter().filter(|item| item.kind == "review") {
        let Some(verdict) = ReviewVerdict::from_state(item.state.as_deref()) else {
            continue;
        };
        match reviewers
            .iter_mut()
            .find(|(author, _)| *author == item.author)
        {
            Some((_, standing)) => {
                if verdict.decisive() || !standing.decisive() {
                    *standing = verdict;
                }
            }
            None => reviewers.push((item.author.clone(), verdict)),
        }
    }
    reviewers
}

/// Passed, failed, and still-running checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChecksRollup {
    pub passed: u64,
    pub failed: u64,
    pub running: u64,
}

impl ChecksRollup {
    pub fn of(pull_request: &PullRequestStatus) -> Self {
        let count = |value: i64| value.max(0) as u64;
        Self {
            passed: count(pull_request.checks_passed),
            failed: count(pull_request.checks_failed),
            running: count(pull_request.checks_pending),
        }
    }

    pub fn total(self) -> u64 {
        self.passed + self.failed + self.running
    }

    /// Every nonzero count, worst first: `1 failed · 2 passed`.
    pub fn summary(self) -> String {
        if self.total() > 0 && self.failed == 0 && self.running == 0 {
            return if self.passed == 1 {
                "Check passed".to_owned()
            } else {
                format!("All {} checks passed", self.passed)
            };
        }
        [
            (self.failed, "failed"),
            (self.running, "running"),
            (self.passed, "passed"),
        ]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect::<Vec<_>>()
        .join(" · ")
    }

    pub fn look(self) -> (IconName, gpui::Rgba) {
        if self.failed > 0 {
            (IconName::CloseCircle, Ink::DANGER)
        } else if self.running > 0 {
            (IconName::Clock, Ink::ATTENTION)
        } else {
            (IconName::CheckCircle, Ink::FRESH)
        }
    }

    /// A rollup that needs a look starts unfolded.
    pub fn opens_by_default(self) -> bool {
        self.failed > 0 || self.running > 0
    }
}

/// Folds the user changed on cards, by pull request URL. A card the user
/// has not touched follows its default.
#[derive(Debug, Default)]
pub struct PrCardState {
    checks_flipped: HashSet<String>,
    discussion_expanded: HashSet<String>,
}

impl PrCardState {
    pub fn checks_open(&self, pull_request: &PullRequestStatus) -> bool {
        ChecksRollup::of(pull_request).opens_by_default()
            != self.checks_flipped.contains(&pull_request.url)
    }

    pub fn toggle_checks(&mut self, url: &str) {
        if !self.checks_flipped.remove(url) {
            self.checks_flipped.insert(url.to_owned());
        }
    }

    pub fn discussion_expanded(&self, url: &str) -> bool {
        self.discussion_expanded.contains(url)
    }

    pub fn toggle_discussion(&mut self, url: &str) {
        if !self.discussion_expanded.remove(url) {
            self.discussion_expanded.insert(url.to_owned());
        }
    }
}

/// One entry of the conversation: a run of one author's consecutive
/// comments, or a bodiless review verdict shown as a timeline event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscussionEntry {
    Thread {
        author: String,
        notes: Vec<usize>,
    },
    Event {
        index: usize,
        verdict: ReviewVerdict,
    },
}

/// Groups the discussion for display. Bodiless plain comments and
/// bodiless "commented" reviews (GitHub's container for inline comments
/// diri does not fetch) carry nothing to read and are dropped.
pub fn thread_discussion(items: &[PrDiscussionItem]) -> Vec<DiscussionEntry> {
    let mut entries: Vec<DiscussionEntry> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let verdict = (item.kind == "review")
            .then(|| ReviewVerdict::from_state(item.state.as_deref()))
            .flatten();
        if item.body.trim().is_empty() {
            match verdict {
                Some(verdict) if verdict != ReviewVerdict::Commented => {
                    entries.push(DiscussionEntry::Event { index, verdict });
                }
                _ => {}
            }
            continue;
        }
        // A verdict always opens its own thread so its badge stays on it.
        if verdict.is_none()
            && let Some(DiscussionEntry::Thread { author, notes }) = entries.last_mut()
            && *author == item.author
            && notes
                .last()
                .is_some_and(|last| items[*last].kind != "review")
        {
            notes.push(index);
            continue;
        }
        entries.push(DiscussionEntry::Thread {
            author: item.author.clone(),
            notes: vec![index],
        });
    }
    entries
}

/// The first entry a card shows: the newest [`DISCUSSION_PREVIEW`] unless
/// expanded.
pub fn first_visible_entry(len: usize, expanded: bool) -> usize {
    if expanded {
        0
    } else {
        len.saturating_sub(DISCUSSION_PREVIEW)
    }
}

pub fn sorted_pr_checks(pull_request: &PullRequestStatus) -> Vec<PrCheck> {
    let mut checks = pull_request.checks.clone().unwrap_or_default();
    checks.sort_by_key(|check| match check.result.as_str() {
        "fail" => 0,
        "pending" => 1,
        "pass" => 2,
        _ => 3,
    });
    checks
}

pub fn pull_request_can_merge(pull_request: &PullRequestStatus) -> bool {
    pull_request.state == "OPEN"
        && !pull_request.is_draft
        && pull_request.mergeable.as_deref() != Some("CONFLICTING")
        && pull_request.checks_failed == 0
        && pull_request.checks_pending == 0
        && !matches!(
            pull_request.review_decision.as_deref(),
            Some("CHANGES_REQUESTED") | Some("REVIEW_REQUIRED")
        )
        && !matches!(
            pull_request.merge_state_status.as_deref(),
            Some("BLOCKED") | Some("DIRTY") | Some("DRAFT")
        )
}

pub fn merge_blocker_label(pull_request: &PullRequestStatus) -> &'static str {
    if pull_request.is_draft {
        "Still a draft"
    } else if pull_request.checks_failed > 0 {
        "Checks are failing"
    } else if pull_request.checks_pending > 0 {
        "Checks are still running"
    } else if pull_request.mergeable.as_deref() == Some("CONFLICTING") {
        "Resolve merge conflicts"
    } else if pull_request.review_decision.as_deref() == Some("CHANGES_REQUESTED") {
        "Changes were requested"
    } else if pull_request.review_decision.as_deref() == Some("REVIEW_REQUIRED") {
        "Review is required"
    } else {
        "GitHub is blocking the merge"
    }
}

pub fn humanize_github_state(value: &str) -> String {
    let lower = value.replace('_', " ").to_ascii_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// Comment and review counts with thread resolution, for a card whose
/// discussion items were not fetched.
pub fn pull_request_discussion(pull_request: &PullRequestStatus) -> Option<String> {
    let plural = |count: i64, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    let mut parts = Vec::new();
    if pull_request.comment_count > 0 {
        parts.push(plural(pull_request.comment_count, "comment", "comments"));
    }
    if pull_request.review_count > 0 {
        parts.push(plural(pull_request.review_count, "review", "reviews"));
    }
    if let Some(total) = pull_request.total_threads.filter(|total| *total > 0) {
        parts.push(format!(
            "{} of {total} threads resolved",
            pull_request.resolved_threads.unwrap_or(0)
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

type Ask = Rc<dyn Fn(Vec<ReviewEvidence>, &mut Window, &mut App)>;
type Select = Rc<dyn Fn(String, QuoteSource, String, &mut Window, &mut App)>;
type Toggle = Rc<dyn Fn(&mut App)>;

/// What the card can ask of its inspector.
#[derive(Clone)]
pub struct PrCardActions {
    pub ask: Ask,
    pub select: Select,
    pub toggle_checks: Toggle,
    pub toggle_discussion: Toggle,
}

/// Everything one card renders from.
pub struct PullRequestCard<'a> {
    pub pull_request: &'a PullRequestStatus,
    pub session_id: SessionId,
    pub body: Option<Arc<MarkdownDocument>>,
    pub selected_key: Option<String>,
    pub checks_open: bool,
    pub discussion_expanded: bool,
    pub actions: PrCardActions,
}

impl PullRequestCard<'_> {
    pub fn render(self, colors: SemanticColors) -> AnyElement {
        let pull_request = self.pull_request;
        let number = pull_request.number;
        let number_label = if number > 0 {
            format!("PR #{number}")
        } else {
            "Pull request".to_owned()
        };
        let title = pull_request
            .title
            .clone()
            .unwrap_or_else(|| number_label.clone());
        let (state_icon, state_tint, state_label) = PullState::of(pull_request).look(colors);
        let rollup = ChecksRollup::of(pull_request);
        let items = pull_request.discussion.as_deref().unwrap_or_default();
        let reviewers = reviewers(items);

        let ask_evidence = ReviewEvidence::PullRequest {
            url: pull_request.url.clone(),
            title: title.clone(),
            body: self.body.as_ref().map(|document| document.plain_text()),
            base: pull_request.base_ref_name.clone(),
            head: pull_request.head_ref_name.clone(),
        };
        let ask = self.actions.ask.clone();
        let view_url = pull_request.url.clone();

        // Title row: state glyph, title with its number and author, actions.
        let byline = match pull_request.author.as_deref() {
            Some(author) if number > 0 => format!("#{number} by {author}"),
            Some(author) => format!("by {author}"),
            None => number_label.clone(),
        };
        let title_row = div()
            .flex()
            .items_start()
            .gap(px(8.0))
            .child(
                div()
                    .pt(px(1.5))
                    .flex_none()
                    .child(icon(state_icon, 16.0, state_tint)),
            )
            .child(
                div()
                    .min_w(px(0.0))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .line_height(px(17.0))
                            .text_size(px(Typo::TITLE.size))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.primary)
                            .child(title.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(Typo::META.size))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(colors.tertiary)
                            .child(byline),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(1.0))
                    .child(
                        ghost_button(
                            SharedString::from(format!("inspector-pr-ask-{number}")),
                            IconName::Sparkle,
                            None,
                            ASK_TINT,
                            rgba(0xd9775722),
                        )
                        .debug_selector(|| "INSPECTOR_PR_ASK".to_owned())
                        .on_click(move |_, window, cx| {
                            ask(vec![ask_evidence.clone()], window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        ghost_button(
                            SharedString::from(format!("inspector-pr-open-{number}")),
                            IconName::ExternalLink,
                            None,
                            colors.tertiary,
                            colors.primary.alpha(0.07),
                        )
                        .debug_selector(|| "INSPECTOR_PR_OPEN".to_owned())
                        .on_click(move |_, _, cx| {
                            cx.open_url(&view_url);
                            cx.stop_propagation();
                        }),
                    ),
            );

        // State and branches: `[Open] head → base`.
        let refs_row = div()
            .min_w(px(0.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(icon_tag(state_icon, state_label, state_tint))
            .when(
                pull_request.head_ref_name.is_some() || pull_request.base_ref_name.is_some(),
                |row| {
                    row.child(ref_tag(
                        pull_request
                            .head_ref_name
                            .clone()
                            .unwrap_or_else(|| "head".to_owned()),
                        colors,
                    ))
                    .child(div().flex_none().child(icon(
                        IconName::ArrowRight,
                        11.0,
                        colors.tertiary,
                    )))
                    .child(
                        div().flex_none().max_w(px(96.0)).child(ref_tag(
                            pull_request
                                .base_ref_name
                                .clone()
                                .unwrap_or_else(|| "base".to_owned()),
                            colors,
                        )),
                    )
                },
            );

        // Size, review standing, and conversation at a glance.
        let files = pull_request.changed_files.max(0);
        let stats_row = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_size(px(Typo::META.size))
            .font_weight(FontWeight::NORMAL)
            .text_color(colors.tertiary)
            .child(diff_stat(
                pull_request.additions.max(0) as u64,
                pull_request.deletions.max(0) as u64,
                colors,
            ))
            .child(div().flex_none().child(format!(
                "{files} {}",
                if files == 1 { "file" } else { "files" }
            )))
            .child(div().flex_1())
            .when(!reviewers.is_empty(), |row| {
                row.child(avatar_group(
                    reviewers.iter().take(4).map(|(name, _)| name.as_str()),
                    16.0,
                ))
            })
            .when_some(review_decision(pull_request), |row, (label, tint)| {
                row.child(tag(label, tint))
            });

        let mut header = div()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(9.0))
            .child(title_row)
            .child(refs_row)
            .child(stats_row);

        if let Some(body) = self.body.clone() {
            let key = format!("pr:{}:body", pull_request.url);
            let selected = self.selected_key.as_deref() == Some(key.as_str());
            let content = body.plain_text();
            let source = QuoteSource::Markdown {
                session_id: self.session_id.clone(),
                document: number_label.clone(),
                turn: 0,
            };
            let select = self.actions.select.clone();
            header = header.child(
                div()
                    .id(SharedString::from(format!("inspector-pr-{number}-body")))
                    .debug_selector(|| "INSPECTOR_PR_BODY".to_owned())
                    .mt(px(2.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(Radius::BADGE))
                    .border_1()
                    .border_color(if selected {
                        selection_stroke()
                    } else {
                        colors.primary.alpha(0.0)
                    })
                    .bg(if selected {
                        selection_fill()
                    } else {
                        colors.primary.alpha(0.03)
                    })
                    .cursor_pointer()
                    .hover(|block| block.bg(selection_hover()))
                    .overflow_hidden()
                    .on_click(move |_, window, cx| {
                        select(key.clone(), source.clone(), content.clone(), window, cx);
                        cx.stop_propagation();
                    })
                    .child(render_markdown(&body, colors)),
            );
        }

        let mut surface = details_ui::card(colors)
            .id(SharedString::from(format!(
                "inspector-pr-{}",
                pull_request.url
            )))
            .flex()
            .flex_col()
            .child(header);

        if rollup.total() > 0 {
            surface = surface
                .child(hairline(colors))
                .child(self.render_checks(rollup, colors));
        }

        let discussion_total = pull_request.comment_count + pull_request.review_count;
        if discussion_total > 0 || !items.is_empty() {
            surface = surface
                .child(hairline(colors))
                .child(self.render_conversation(items, colors));
        }

        if pull_request.state == "OPEN" {
            surface = surface
                .child(hairline(colors))
                .child(render_merge_footer(pull_request, colors));
        }

        surface.into_any_element()
    }

    fn render_checks(&self, rollup: ChecksRollup, colors: SemanticColors) -> AnyElement {
        let number = self.pull_request.number;
        let open = self.checks_open;
        let (rollup_icon, rollup_tint) = rollup.look();
        let toggle = self.actions.toggle_checks.clone();
        let checks = sorted_pr_checks(self.pull_request);
        let can_open = !checks.is_empty();

        let summary = div()
            .id(SharedString::from(format!("inspector-pr-{number}-checks")))
            .debug_selector(|| "INSPECTOR_PR_CHECKS_TOGGLE".to_owned())
            .h(px(36.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(can_open, |row| {
                row.cursor_pointer()
                    .hover(move |row| row.bg(colors.primary.alpha(0.035)))
                    .on_click(move |_, _, cx| {
                        toggle(cx);
                        cx.stop_propagation();
                    })
            })
            .child(icon(rollup_icon, 14.0, rollup_tint))
            .child(
                div()
                    .min_w(px(0.0))
                    .flex_1()
                    .truncate()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.primary.alpha(0.86))
                    .child(rollup.summary()),
            )
            .when(can_open, |row| {
                row.child(icon(
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    12.0,
                    colors.tertiary,
                ))
            });

        let mut block = div().flex().flex_col().child(summary);
        if open && can_open {
            let mut rows = div()
                .mx(px(12.0))
                .mb(px(10.0))
                .rounded(px(Radius::BADGE))
                .bg(colors.primary.alpha(0.025))
                .overflow_hidden()
                .flex()
                .flex_col();
            for (index, check) in checks.iter().enumerate() {
                if index > 0 {
                    rows = rows.child(hairline(colors));
                }
                rows = rows.child(render_check(check, index, number, &self.actions, colors));
            }
            block = block.child(rows);
        }
        block.into_any_element()
    }

    fn render_conversation(
        &self,
        items: &[PrDiscussionItem],
        colors: SemanticColors,
    ) -> AnyElement {
        let pull_request = self.pull_request;
        let entries = thread_discussion(items);
        let first = first_visible_entry(entries.len(), self.discussion_expanded);
        let count = if items.is_empty() {
            (pull_request.comment_count + pull_request.review_count).max(0) as usize
        } else {
            items.len()
        };
        let resolved = pull_request
            .total_threads
            .filter(|total| *total > 0)
            .map(|total| {
                format!(
                    "{}/{total} resolved",
                    pull_request.resolved_threads.unwrap_or(0)
                )
            });

        let mut block = div()
            .px(px(12.0))
            .pt(px(10.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(Typo::SECTION_HEADER.size))
                    .child(
                        div()
                            .font_weight(Typo::SECTION_HEADER.weight)
                            .text_color(colors.secondary)
                            .child("Conversation"),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(colors.tertiary)
                            .child(count.to_string()),
                    )
                    .when_some(resolved, |header, resolved| {
                        header.child(
                            div()
                                .ml_auto()
                                .font_weight(FontWeight::NORMAL)
                                .text_color(colors.tertiary)
                                .child(resolved),
                        )
                    }),
            );

        if entries.is_empty() {
            return block
                .child(render_discussion_fallback(pull_request, colors))
                .into_any_element();
        }

        if first > 0 || self.discussion_expanded {
            let toggle = self.actions.toggle_discussion.clone();
            let label = if self.discussion_expanded {
                "Show fewer".to_owned()
            } else {
                format!(
                    "Show {first} earlier {}",
                    if first == 1 { "entry" } else { "entries" }
                )
            };
            block = block.child(
                div()
                    .id(SharedString::from(format!(
                        "inspector-pr-{}-earlier",
                        pull_request.number
                    )))
                    .debug_selector(|| "INSPECTOR_PR_DISCUSSION_TOGGLE".to_owned())
                    .self_start()
                    .h(px(20.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .rounded(px(Radius::CHIP))
                    .cursor_pointer()
                    .hover(move |button| button.bg(colors.primary.alpha(0.06)))
                    .text_size(px(Typo::META.size))
                    .text_color(colors.secondary)
                    .child(label)
                    .on_click(move |_, _, cx| {
                        toggle(cx);
                        cx.stop_propagation();
                    }),
            );
        }

        for entry in &entries[first..] {
            block = block.child(match entry {
                DiscussionEntry::Thread { author, notes } => {
                    self.render_thread(author, notes, items, colors)
                }
                DiscussionEntry::Event { index, verdict } => {
                    self.render_event(*index, verdict, &items[*index], colors)
                }
            });
        }
        block.into_any_element()
    }

    /// Ely's `ReviewComment`: one author's avatar beside their notes, each
    /// note its own selectable quote.
    fn render_thread(
        &self,
        author: &str,
        notes: &[usize],
        items: &[PrDiscussionItem],
        colors: SemanticColors,
    ) -> AnyElement {
        let lead = &items[notes[0]];
        let verdict = (lead.kind == "review")
            .then(|| ReviewVerdict::from_state(lead.state.as_deref()))
            .flatten();
        let time = lead.created_at.as_ref().map(|date| relative_time(date.0));
        let mut column = div()
            .min_w(px(0.0))
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex_shrink(1.0)
                            .truncate()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.primary)
                            .child(author.to_owned()),
                    )
                    .when_some(verdict, |header, verdict| {
                        header.child(tag(verdict.label(), verdict.tone(colors)))
                    })
                    .when_some(time, |header, time| {
                        header.child(
                            div()
                                .flex_none()
                                .text_size(px(10.5))
                                .text_color(colors.tertiary)
                                .child(time),
                        )
                    }),
            );
        for &index in notes {
            column = column.child(self.render_note(index, &items[index], colors));
        }
        div()
            .flex()
            .items_start()
            .gap(px(8.0))
            .child(avatar(author, 20.0))
            .child(column)
            .into_any_element()
    }

    fn render_note(
        &self,
        index: usize,
        item: &PrDiscussionItem,
        colors: SemanticColors,
    ) -> AnyElement {
        let body = MarkdownDocument::parse(&item.body);
        let content = body.plain_text();
        let url = item.url.clone();
        let key = discussion_key(index, item);
        let selected = self.selected_key.as_deref() == Some(key.as_str());
        let source = QuoteSource::Markdown {
            session_id: self.session_id.clone(),
            document: format!("pull request discussion by {}", item.author),
            turn: index,
        };
        let select = self.actions.select.clone();
        div()
            .id(SharedString::from(format!("inspector-pr-comment-{index}")))
            .debug_selector(move || format!("INSPECTOR_PR_COMMENT_{index}"))
            .group(SharedString::from(format!("inspector-pr-note-{index}")))
            .relative()
            .ml(px(-6.0))
            .px(px(6.0))
            .py(px(4.0))
            .rounded(px(Radius::BADGE))
            .border_1()
            .border_color(if selected {
                selection_stroke()
            } else {
                colors.primary.alpha(0.0)
            })
            .when(selected, |note| note.bg(selection_fill()))
            .cursor_pointer()
            .hover(|note| note.bg(selection_hover()))
            .on_click(move |_, window, cx| {
                select(key.clone(), source.clone(), content.clone(), window, cx);
                cx.stop_propagation();
            })
            .child(render_markdown(&body, colors))
            .when_some(url, |note, url| {
                note.child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .right(px(2.0))
                        .invisible()
                        .group_hover(
                            SharedString::from(format!("inspector-pr-note-{index}")),
                            |button| button.visible(),
                        )
                        .child(
                            ghost_button(
                                ("open-discussion-item", index),
                                IconName::ExternalLink,
                                None,
                                colors.tertiary,
                                colors.primary.alpha(0.07),
                            )
                            .on_click(move |_, _, cx| {
                                cx.open_url(&url);
                                cx.stop_propagation();
                            }),
                        ),
                )
            })
            .into_any_element()
    }

    /// A bodiless review: one quiet timeline line.
    fn render_event(
        &self,
        index: usize,
        verdict: &ReviewVerdict,
        item: &PrDiscussionItem,
        colors: SemanticColors,
    ) -> AnyElement {
        let tint = verdict.tone(colors);
        let key = discussion_key(index, item);
        let selected = self.selected_key.as_deref() == Some(key.as_str());
        let content = verdict.label();
        let source = QuoteSource::Markdown {
            session_id: self.session_id.clone(),
            document: format!("pull request discussion by {}", item.author),
            turn: index,
        };
        let select = self.actions.select.clone();
        let time = item.created_at.as_ref().map(|date| relative_time(date.0));
        div()
            .id(SharedString::from(format!("inspector-pr-comment-{index}")))
            .debug_selector(move || format!("INSPECTOR_PR_COMMENT_{index}"))
            .min_h(px(24.0))
            .ml(px(-6.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(Radius::BADGE))
            .when(selected, |row| row.bg(selection_fill()))
            .cursor_pointer()
            .hover(|row| row.bg(selection_hover()))
            .on_click(move |_, window, cx| {
                select(key.clone(), source.clone(), content.clone(), window, cx);
                cx.stop_propagation();
            })
            .child(
                div()
                    .w(px(20.0))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(icon(verdict.icon(), 14.0, tint)),
            )
            .child(
                div()
                    .min_w(px(0.0))
                    .flex_1()
                    .truncate()
                    .text_size(px(Typo::META.size))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(colors.secondary)
                    .child(format!("{} {}", item.author, verdict.event_phrase())),
            )
            .when_some(time, |row, time| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(px(10.5))
                        .text_color(colors.tertiary)
                        .child(time),
                )
            })
            .into_any_element()
    }
}

/// The selection key of a discussion item; stable across renders.
fn discussion_key(index: usize, item: &PrDiscussionItem) -> String {
    format!(
        "discussion:{index}:{}",
        item.url.as_deref().unwrap_or("local")
    )
}

fn render_check(
    check: &PrCheck,
    index: usize,
    pr_number: i64,
    actions: &PrCardActions,
    colors: SemanticColors,
) -> AnyElement {
    let (glyph, tint, status) = match check.result.as_str() {
        "pass" => (IconName::CheckCircle, Ink::FRESH, "Passed"),
        "fail" => (IconName::CloseCircle, Ink::DANGER, "Failed"),
        "pending" => (IconName::Clock, Ink::ATTENTION, "Running"),
        _ => (IconName::Info, colors.tertiary, "Unknown"),
    };
    let detail = check
        .detail
        .as_deref()
        .map(humanize_github_state)
        .filter(|detail| detail != status && detail != "Success")
        .unwrap_or_else(|| status.to_owned());
    let url = check.url.clone();
    let ask = actions.ask.clone();
    let ask_evidence = ReviewEvidence::Check {
        name: check.name.clone(),
        result: check.result.clone(),
        detail: check.detail.clone(),
    };
    let group = SharedString::from(format!("inspector-pr-{pr_number}-check-row-{index}"));
    div()
        .id(SharedString::from(format!(
            "inspector-pr-{pr_number}-check-{index}"
        )))
        .debug_selector(move || format!("INSPECTOR_PR_CHECK_{index}"))
        .group(group.clone())
        .min_h(px(30.0))
        .pl(px(9.0))
        .pr(px(4.0))
        .flex()
        .items_center()
        .gap(px(7.0))
        .when(url.is_some(), |row| {
            row.cursor_pointer()
                .hover(move |row| row.bg(colors.primary.alpha(0.04)))
        })
        .child(icon(glyph, 12.0, tint))
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .truncate()
                .text_size(px(Typo::META.size))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.primary.alpha(0.82))
                .child(check.name.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(10.5))
                .font_weight(FontWeight::NORMAL)
                .text_color(if check.result == "pass" {
                    colors.tertiary
                } else {
                    tint
                })
                .child(detail),
        )
        .child(
            ghost_button(
                ("ask-pr-check", index),
                IconName::Sparkle,
                None,
                ASK_TINT,
                rgba(0xd9775722),
            )
            .on_click(move |_, window, cx| {
                ask(vec![ask_evidence.clone()], window, cx);
                cx.stop_propagation();
            }),
        )
        .when_some(url, |row, url| {
            row.on_click(move |_, _, cx| cx.open_url(&url))
        })
        .into_any_element()
}

fn render_discussion_fallback(
    pull_request: &PullRequestStatus,
    colors: SemanticColors,
) -> AnyElement {
    let discussion = pull_request_discussion(pull_request)
        .unwrap_or_else(|| "Open the conversation on GitHub".to_owned());
    let url = pull_request.url.clone();
    div()
        .id(SharedString::from(format!(
            "inspector-pr-discussion-{}",
            pull_request.number
        )))
        .min_h(px(30.0))
        .ml(px(-6.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(Radius::BADGE))
        .cursor_pointer()
        .hover(move |row| row.bg(colors.primary.alpha(0.05)))
        .child(icon(IconName::Comment, 14.0, colors.secondary))
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .text_size(px(Typo::META.size))
                .text_color(colors.secondary)
                .child(discussion),
        )
        .child(icon(IconName::ExternalLink, 12.0, colors.tertiary))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .into_any_element()
}

fn render_merge_footer(pull_request: &PullRequestStatus, colors: SemanticColors) -> Div {
    let can_merge = pull_request_can_merge(pull_request);
    let (label, tint) = if can_merge {
        ("Ready to merge", Ink::FRESH)
    } else {
        (merge_blocker_label(pull_request), Ink::ATTENTION)
    };
    let merge_url = pull_request.url.clone();
    let button_text = if can_merge {
        gpui::Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        }
    } else {
        colors.secondary
    };
    div()
        .px(px(12.0))
        .py(px(9.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(div().size(px(7.0)).flex_none().rounded_full().bg(tint))
        .child(
            div()
                .min_w(px(0.0))
                .flex_1()
                .truncate()
                .text_size(px(Typo::META.size))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.primary.alpha(0.86))
                .child(label),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "inspector-pr-merge-{}",
                    pull_request.number
                )))
                .debug_selector(|| "INSPECTOR_PR_MERGE".to_owned())
                .h(px(24.0))
                .px(px(9.0))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(5.0))
                .rounded(px(Radius::CHIP))
                .cursor_pointer()
                .bg(if can_merge {
                    Ink::FRESH.alpha(0.88)
                } else {
                    colors.primary.alpha(0.07)
                })
                .hover(move |button| {
                    button.bg(if can_merge {
                        Ink::FRESH
                    } else {
                        colors.primary.alpha(0.11)
                    })
                })
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(button_text)
                .child("Merge")
                .child(icon(IconName::ExternalLink, 11.0, button_text))
                .on_click(move |_, _, cx| cx.open_url(&merge_url)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use diri_proto::DateMillis;

    fn item(kind: &str, author: &str, body: &str, state: Option<&str>) -> PrDiscussionItem {
        PrDiscussionItem {
            kind: kind.to_owned(),
            author: author.to_owned(),
            body: body.to_owned(),
            state: state.map(str::to_owned),
            created_at: Some(DateMillis(0.0)),
            url: None,
        }
    }

    fn status(state: &str, draft: bool) -> PullRequestStatus {
        PullRequestStatus {
            url: "https://github.com/acme/diri/pull/7".to_owned(),
            number: 7,
            title: None,
            author: None,
            body: None,
            base_ref_name: None,
            head_ref_name: None,
            state: state.to_owned(),
            is_draft: draft,
            review_decision: None,
            mergeable: None,
            merge_state_status: None,
            additions: 0,
            deletions: 0,
            changed_files: 0,
            comment_count: 0,
            review_count: 0,
            resolved_threads: None,
            total_threads: None,
            checks_passed: 0,
            checks_failed: 0,
            checks_pending: 0,
            checks: None,
            discussion: None,
            fetched_at: DateMillis(0.0),
        }
    }

    #[test]
    fn pull_state_maps_github_state_and_draft() {
        assert_eq!(PullState::of(&status("OPEN", false)), PullState::Open);
        assert_eq!(PullState::of(&status("OPEN", true)), PullState::Draft);
        assert_eq!(PullState::of(&status("MERGED", false)), PullState::Merged);
        assert_eq!(PullState::of(&status("CLOSED", true)), PullState::Closed);
    }

    #[test]
    fn checks_rollup_summarizes_worst_first_and_opens_when_attention_is_needed() {
        let mut pull_request = status("OPEN", false);
        assert_eq!(ChecksRollup::of(&pull_request).total(), 0);

        pull_request.checks_passed = 2;
        pull_request.checks_pending = 1;
        let rollup = ChecksRollup::of(&pull_request);
        assert_eq!(rollup.summary(), "1 running · 2 passed");
        assert!(rollup.opens_by_default());
        assert_eq!(rollup.look().1, Ink::ATTENTION);

        pull_request.checks_failed = 1;
        let rollup = ChecksRollup::of(&pull_request);
        assert_eq!(rollup.summary(), "1 failed · 1 running · 2 passed");
        assert_eq!(rollup.look().1, Ink::DANGER);

        pull_request.checks_failed = 0;
        pull_request.checks_pending = 0;
        let rollup = ChecksRollup::of(&pull_request);
        assert_eq!(rollup.summary(), "All 2 checks passed");
        assert!(!rollup.opens_by_default());

        pull_request.checks_passed = 1;
        assert_eq!(ChecksRollup::of(&pull_request).summary(), "Check passed");
        pull_request.checks_passed = -3;
        assert_eq!(
            ChecksRollup::of(&pull_request).total(),
            0,
            "negative counts clamp"
        );
    }

    #[test]
    fn card_folds_follow_defaults_until_the_user_flips_them() {
        let mut state = PrCardState::default();
        let mut pull_request = status("OPEN", false);
        pull_request.checks_passed = 3;
        assert!(!state.checks_open(&pull_request));
        state.toggle_checks(&pull_request.url);
        assert!(state.checks_open(&pull_request));

        // A failure flips the default; the user's flip stays a flip.
        pull_request.checks_failed = 1;
        assert!(!state.checks_open(&pull_request));
        state.toggle_checks(&pull_request.url);
        assert!(state.checks_open(&pull_request));

        assert!(!state.discussion_expanded(&pull_request.url));
        state.toggle_discussion(&pull_request.url);
        assert!(state.discussion_expanded(&pull_request.url));
        state.toggle_discussion(&pull_request.url);
        assert!(!state.discussion_expanded(&pull_request.url));
    }

    #[test]
    fn review_verdicts_map_and_reviewers_keep_their_decisive_verdict() {
        assert_eq!(
            ReviewVerdict::from_state(Some("CHANGES_REQUESTED")),
            Some(ReviewVerdict::ChangesRequested)
        );
        assert_eq!(ReviewVerdict::from_state(None), None);
        assert_eq!(
            ReviewVerdict::from_state(Some("SOMETHING_NEW")).map(|verdict| verdict.label()),
            Some("Something new".to_owned())
        );

        let items = vec![
            item("review", "ana", "", Some("CHANGES_REQUESTED")),
            item("comment", "bo", "hi", None),
            item("review", "bo", "lgtm", Some("COMMENTED")),
            item("review", "ana", "ok", Some("COMMENTED")),
            item("review", "bo", "", Some("APPROVED")),
        ];
        assert_eq!(
            reviewers(&items),
            vec![
                ("ana".to_owned(), ReviewVerdict::ChangesRequested),
                ("bo".to_owned(), ReviewVerdict::Approved),
            ]
        );
    }

    #[test]
    fn review_decision_names_only_known_requirements() {
        let mut pull_request = status("OPEN", false);
        assert_eq!(review_decision(&pull_request), None);
        pull_request.review_decision = Some("REVIEW_REQUIRED".to_owned());
        assert_eq!(
            review_decision(&pull_request).map(|(label, _)| label),
            Some("Review required")
        );
        pull_request.review_decision = Some("WHATEVER".to_owned());
        assert_eq!(review_decision(&pull_request), None);
    }

    #[test]
    fn discussion_threads_runs_of_one_author_and_turns_bodiless_verdicts_into_events() {
        let items = vec![
            item("comment", "ruru", "first", None),
            item("comment", "ruru", "second", None),
            item("comment", "antfu", "reply", None),
            item("review", "evan", "", Some("COMMENTED")),
            item("review", "evan", "", Some("APPROVED")),
            item("comment", "evan", "  ", None),
            item("review", "antfu", "looks good", Some("APPROVED")),
            item("comment", "antfu", "merging", None),
        ];
        assert_eq!(
            thread_discussion(&items),
            vec![
                DiscussionEntry::Thread {
                    author: "ruru".to_owned(),
                    notes: vec![0, 1],
                },
                DiscussionEntry::Thread {
                    author: "antfu".to_owned(),
                    notes: vec![2],
                },
                DiscussionEntry::Event {
                    index: 4,
                    verdict: ReviewVerdict::Approved,
                },
                // A verdict opens its own thread; a comment after a review
                // does not join it.
                DiscussionEntry::Thread {
                    author: "antfu".to_owned(),
                    notes: vec![6],
                },
                DiscussionEntry::Thread {
                    author: "antfu".to_owned(),
                    notes: vec![7],
                },
            ]
        );
    }

    #[test]
    fn folded_conversation_shows_the_newest_entries() {
        assert_eq!(first_visible_entry(3, false), 0);
        assert_eq!(first_visible_entry(DISCUSSION_PREVIEW, false), 0);
        assert_eq!(first_visible_entry(10, false), 10 - DISCUSSION_PREVIEW);
        assert_eq!(first_visible_entry(10, true), 0);
    }

    #[test]
    fn merge_gate_names_the_first_blocker() {
        let mut pull_request = status("OPEN", false);
        assert!(pull_request_can_merge(&pull_request));
        pull_request.is_draft = true;
        assert!(!pull_request_can_merge(&pull_request));
        assert_eq!(merge_blocker_label(&pull_request), "Still a draft");
        pull_request.is_draft = false;
        pull_request.mergeable = Some("CONFLICTING".to_owned());
        assert_eq!(
            merge_blocker_label(&pull_request),
            "Resolve merge conflicts"
        );
        pull_request.checks_failed = 1;
        assert_eq!(merge_blocker_label(&pull_request), "Checks are failing");
    }

    #[test]
    fn discussion_summary_counts_and_resolution() {
        let mut pull_request = status("OPEN", false);
        assert_eq!(pull_request_discussion(&pull_request), None);
        pull_request.comment_count = 1;
        pull_request.review_count = 2;
        pull_request.total_threads = Some(3);
        pull_request.resolved_threads = Some(1);
        assert_eq!(
            pull_request_discussion(&pull_request).as_deref(),
            Some("1 comment · 2 reviews · 1 of 3 threads resolved")
        );
    }
}
