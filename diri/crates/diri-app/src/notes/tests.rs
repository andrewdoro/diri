use std::sync::Arc;

use diri_notes::markdown;
use diri_notes::store::NoteStore;
use diri_proto::SessionId;
use gpui::EntityInputHandler as _;

use super::*;
use crate::store::StoreRuntime;

pub(crate) const PLAN: &str = "# Notes launch plan

A **rich**, _calm_ place to think next to your agents. Everything is plain Markdown on disk — see [the spec](https://diri.sh/notes).

## This week

- [x] Block model and Markdown codec
- [x] Atomic file store
- [ ] Quick capture from anywhere
- [ ] Agents can `append` to a note

## Open questions

> Should to-dos roll up into a Today view, or stay inside their notes?

1. Keep files flat
2. Organise with front matter

```
dirijor note \"ship it\" --project .
```
";

fn store_with_plan() -> (tempfile::TempDir, Arc<NoteStore>, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = NoteStore::open(dir.path().join("notes")).expect("store");
    let (_, doc) = markdown::parse(PLAN);
    let (id, _) = store.create(doc, None).expect("create");
    (dir, Arc::new(store), id)
}

fn pane(
    cx: &mut gpui::TestAppContext,
    store: Arc<NoteStore>,
) -> (Entity<NotePane>, &mut gpui::VisualTestContext) {
    let runtime = Arc::new(StoreRuntime::inert());
    cx.add_window_view(move |_, cx| NotePane::with_store(runtime, Some(store), false, cx))
}

fn editor(pane: &Entity<NotePane>, cx: &mut gpui::VisualTestContext) -> Entity<NoteEditorView> {
    pane.read_with(cx, |pane, _| match &pane.state {
        PaneState::Open(open) => open.editor.clone(),
        _ => panic!("note is not open"),
    })
}

#[gpui::test]
fn typing_saves_markdown_and_keeps_formatting(cx: &mut gpui::TestAppContext) {
    let (_dir, store, id) = store_with_plan();
    let (pane, cx) = pane(cx, store.clone());
    let session = SessionId::new("s_note");
    pane.update_in(cx, |pane, window, cx| pane.show(&session, &id, window, cx));
    let editor = editor(&pane, cx);
    editor.update_in(cx, |view, window, cx| {
        // End of the note, then a new to-do typed with the Markdown shortcut.
        let last = view.editor.blocks().len() - 1;
        view.editor.set_caret(diri_notes::edit::Pos::new(last, 0));
        for ch in "[] ship **notes**".chars() {
            view.replace_text_in_range(None, &ch.to_string(), window, cx);
        }
    });
    pane.update(cx, |pane, cx| pane.save(cx));
    let text = std::fs::read_to_string(store.path_for(&id).unwrap()).unwrap();
    assert!(text.ends_with("- [ ] ship **notes**\n"), "{text}");
    assert!(text.contains("# Notes launch plan"));
}

#[gpui::test]
fn outside_writes_reload_a_clean_note(cx: &mut gpui::TestAppContext) {
    let (_dir, store, id) = store_with_plan();
    let (pane, cx) = pane(cx, store.clone());
    let session = SessionId::new("s_note");
    pane.update_in(cx, |pane, window, cx| pane.show(&session, &id, window, cx));
    // An agent appends through the CLI while the note is open and untouched.
    store.append(&id, "- [ ] added by an agent").unwrap();
    pane.update(cx, |pane, cx| pane.reconcile(cx));
    let editor = editor(&pane, cx);
    let has = editor.read_with(cx, |view, _| {
        view.editor
            .blocks()
            .iter()
            .any(|b| b.text == "added by an agent")
    });
    assert!(has, "open note reloads outside edits");
}

#[gpui::test]
fn a_missing_file_shows_an_explanation_not_a_crash(cx: &mut gpui::TestAppContext) {
    let (_dir, store, _) = store_with_plan();
    let (pane, cx) = pane(cx, store);
    let session = SessionId::new("s_note");
    pane.update_in(cx, |pane, window, cx| {
        pane.show(&session, "20990101-000000-dead", window, cx)
    });
    pane.read_with(cx, |pane, _| {
        assert!(matches!(pane.state, PaneState::Missing { .. }));
    });
}

/// Sessions and a note a fixture editor can mention, one session per status
/// a chip draws.
pub(crate) fn fixture_mentions() -> Vec<editor_view::MentionEntry> {
    use diri_notes::mention::{Candidate, MentionTarget, note_label, session_label};
    use diri_ui::{AgentKind, StatusState};
    let mut entries: Vec<editor_view::MentionEntry> = [
        (
            "s_codex",
            AgentKind::Codex,
            "Codex",
            "fix resize flicker",
            StatusState::Working,
            "diri",
        ),
        (
            "s_claude",
            AgentKind::ClaudeCode,
            "Claude Code",
            "draft launch email",
            StatusState::NeedsInput { destructive: false },
            "Growth",
        ),
        (
            "s_gemini",
            AgentKind::Gemini,
            "Gemini",
            "release notes draft",
            StatusState::DoneUnseen,
            "diri-web",
        ),
        (
            "s_shell",
            AgentKind::Shell,
            "Terminal",
            "cargo test",
            StatusState::IdleSeen,
            "~/fun/diri",
        ),
    ]
    .into_iter()
    .map(
        |(id, agent, name, title, status, detail)| editor_view::MentionEntry {
            candidate: Candidate {
                target: MentionTarget::Session(id.into()),
                label: session_label(name, title),
                keywords: name.to_lowercase(),
            },
            agent: Some(agent),
            status: Some(status),
            detail: detail.into(),
        },
    )
    .collect();
    for (id, title, project) in [
        ("n-groceries", "Groceries", ""),
        ("n-q4", "Q4 campaign brief", "Growth"),
    ] {
        entries.push(editor_view::MentionEntry {
            candidate: Candidate {
                target: MentionTarget::Note(id.into()),
                label: note_label(title),
                keywords: "note".into(),
            },
            agent: None,
            status: None,
            detail: project.into(),
        });
    }
    entries
}

#[gpui::test]
fn at_mentions_insert_session_and_note_links(cx: &mut gpui::TestAppContext) {
    let (_dir, store, id) = store_with_plan();
    let (pane, cx) = pane(cx, store.clone());
    let session = SessionId::new("s_note");
    pane.update_in(cx, |pane, window, cx| pane.show(&session, &id, window, cx));
    let editor = editor(&pane, cx);
    editor.update(cx, |view, cx| {
        view.set_mentions(
            editor_view::MentionDirectory {
                entries: fixture_mentions(),
            },
            cx,
        )
    });
    fn type_str(
        view: &mut NoteEditorView,
        text: &str,
        window: &mut Window,
        cx: &mut Context<NoteEditorView>,
    ) {
        for ch in text.chars() {
            view.replace_text_in_range(None, &ch.to_string(), window, cx);
        }
    }
    editor.update_in(cx, |view, window, cx| {
        let last = view.editor.blocks().len() - 1;
        view.editor.set_caret(diri_notes::edit::Pos::new(last, 0));
        type_str(view, "wait for @re", window, cx);
        // Two sessions match "re"; arrow to the second and take it with Tab.
        assert_eq!(
            view.mention_matches().len(),
            2,
            "codex resize + gemini release"
        );
        view.vertical(true, false, cx);
        view.indent(&editor_view::Indent, window, cx);
        type_str(view, "then @groc", window, cx);
        view.newline(&editor_view::Newline, window, cx);
        // An email address never opens the menu.
        type_str(view, "mail me@x", window, cx);
        assert!(!view.mention_open());
    });
    pane.update(cx, |pane, cx| pane.save(cx));
    let text = std::fs::read_to_string(store.path_for(&id).unwrap()).unwrap();
    assert!(
        text.contains(
            "wait for [@Gemini: release notes draft](diri://session/s_gemini) then [@Groceries](diri://note/n-groceries) mail me@x"
        ),
        "{text}"
    );
    let (_, doc) = markdown::parse(&text);
    assert_eq!(
        doc.mentions(),
        vec![
            diri_notes::mention::MentionTarget::Session("s_gemini".into()),
            diri_notes::mention::MentionTarget::Note("n-groceries".into()),
        ]
    );
}

#[gpui::test]
fn hover_and_arrow_keys_share_one_menu_highlight(cx: &mut gpui::TestAppContext) {
    let (_dir, store, id) = store_with_plan();
    let (pane, cx) = pane(cx, store);
    let session = SessionId::new("s_note");
    pane.update_in(cx, |pane, window, cx| pane.show(&session, &id, window, cx));
    let editor = editor(&pane, cx);
    editor.update_in(cx, |view, window, cx| {
        let last = view.editor.blocks().len() - 1;
        view.editor.set_caret(diri_notes::edit::Pos::new(last, 0));
        view.replace_text_in_range(None, "/", window, cx);
        assert_eq!(view.menu_selected(), Some(0));
        view.vertical(true, false, cx);
        assert_eq!(view.menu_selected(), Some(1));
        // The pointer takes the same highlight the arrows moved...
        view.hover_menu_row(4, cx);
        assert_eq!(view.menu_selected(), Some(4));
        // ...and the arrows continue from where the pointer left it.
        view.vertical(true, false, cx);
        assert_eq!(view.menu_selected(), Some(5));
        // Enter applies the one highlighted row: Bulleted list.
        view.newline(&editor_view::Newline, window, cx);
        assert_eq!(
            view.editor.block(last).kind,
            diri_notes::doc::BlockKind::Bullet
        );
    });
}

#[gpui::test]
fn folding_is_view_state_that_survives_outside_writes(cx: &mut gpui::TestAppContext) {
    let (_dir, store, id) = store_with_plan();
    let (pane, cx) = pane(cx, store.clone());
    let session = SessionId::new("s_note");
    pane.update_in(cx, |pane, window, cx| pane.show(&session, &id, window, cx));
    let editor = editor(&pane, cx);
    let parent = editor.update_in(cx, |view, window, cx| {
        let parent = view
            .editor
            .blocks()
            .iter()
            .position(|b| b.text == "Quick capture from anywhere")
            .unwrap();
        let end = view.editor.block(parent).text.len();
        view.editor
            .set_caret(diri_notes::edit::Pos::new(parent, end));
        view.newline(&editor_view::Newline, window, cx);
        view.indent(&editor_view::Indent, window, cx);
        for ch in "a global hotkey".chars() {
            view.replace_text_in_range(None, &ch.to_string(), window, cx);
        }
        // ⌥⌘↩ inside the child folds the item that holds it.
        view.toggle_fold(&editor_view::ToggleFold, window, cx);
        assert!(view.editor.is_collapsed(parent));
        assert!(view.editor.is_hidden(parent + 1));
        assert_eq!(view.editor.selection.head.block, parent);
        parent
    });
    pane.update(cx, |pane, cx| pane.save(cx));
    let text = std::fs::read_to_string(store.path_for(&id).unwrap()).unwrap();
    assert!(
        text.contains("- [ ] Quick capture from anywhere\n  - [ ] a global hotkey\n"),
        "folding never changes the file:\n{text}"
    );
    // An agent appends while the item is folded; the fold survives the reload.
    store.append(&id, "- [ ] added by an agent").unwrap();
    pane.update(cx, |pane, cx| pane.reconcile(cx));
    editor.read_with(cx, |view, _| {
        assert!(view.editor.is_collapsed(parent));
        assert!(
            view.editor
                .blocks()
                .iter()
                .any(|b| b.text == "added by an agent")
        );
    });
}
