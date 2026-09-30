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
