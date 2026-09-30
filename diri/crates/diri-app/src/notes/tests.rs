use std::path::PathBuf;
use std::sync::Arc;

use diri_notes::markdown;
use diri_notes::store::{self, NoteStore};
use gpui::{AppContext as _, Entity, EntityInputHandler as _, HeadlessAppContext, px, size};

use super::*;
use crate::store::StoreRuntime;

const PLAN: &str = "# Notes launch plan

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

const GROCERIES: &str = "# Groceries

- [ ] Oat milk
- [ ] Coffee beans
- [x] Bread
";

const IDEAS: &str = "# Ideas for the sidebar

Things 3 keeps *areas* calm: a handful of fixed places and nothing to prune.

- Pinned notes lead their list
- Empty notes vanish
";

fn fixture_store() -> (tempfile::TempDir, Arc<NoteStore>, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = NoteStore::open(dir.path().join("notes")).expect("store");
    let mut plan_id = String::new();
    for (i, (source, project, pinned)) in [
        (IDEAS, None, false),
        (GROCERIES, None, false),
        (PLAN, None, true),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, doc) = markdown::parse(source);
        let (id, mut note) = store.create(doc, project).expect("create");
        if pinned {
            note.front.set_flag(store::KEY_PINNED, true);
            store.save(&id, &note).expect("save");
            plan_id = id.clone();
        }
        // Distinct modification times keep the list order deterministic.
        std::thread::sleep(std::time::Duration::from_millis(15 + i as u64));
    }
    (dir, Arc::new(store), plan_id)
}

fn open_view(
    cx: &mut HeadlessAppContext,
    store: Arc<NoteStore>,
    theme: &str,
) -> gpui::WindowHandle<NotesView> {
    let runtime = Arc::new(StoreRuntime::inert());
    let theme = theme.to_owned();
    cx.open_window(size(px(1180.0), px(760.0)), move |window, cx| {
        cx.new(|cx| {
            let mut view = NotesView::new(runtime, store, window, cx);
            view.theme_override = Some((
                crate::app_theme::colors(&theme),
                crate::app_theme::sidebar_colors(&theme),
            ));
            view
        })
    })
    .expect("open notes window")
}

fn headless() -> HeadlessAppContext {
    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(diri_ui::IconAssets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        crate::fonts::init(cx);
        cx.bind_keys(key_bindings());
    });
    cx
}

fn editor_of(
    cx: &mut HeadlessAppContext,
    window: gpui::WindowHandle<NotesView>,
) -> Entity<NoteEditorView> {
    cx.update_window(window.into(), |root, _, cx| {
        let view = root.downcast::<NotesView>().expect("notes view");
        view.read(cx)
            .open
            .as_ref()
            .expect("open note")
            .editor
            .clone()
    })
    .expect("window")
}

/// Renders the Notes window from fixture notes. Scenes (DIRI_NOTES_SCENE):
/// `editor` (default), `slash`, `todos`, `empty`. Theme: DIRI_NOTES_THEME.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "writes the notes screenshot artifact"]
fn render_notes_screenshot() {
    let output = std::env::var_os("DIRI_VISUAL_OUTPUT")
        .map(PathBuf::from)
        .expect("set DIRI_VISUAL_OUTPUT to the target PNG path");
    let scene = std::env::var("DIRI_NOTES_SCENE").unwrap_or_else(|_| "editor".into());
    let theme = std::env::var("DIRI_NOTES_THEME").unwrap_or_else(|_| "dirijor-light".into());
    let mut cx = headless();
    let (_dir, store, plan) = fixture_store();
    let store = if scene == "empty" {
        Arc::new(NoteStore::open(_dir.path().join("empty")).expect("store"))
    } else {
        store
    };
    let window = open_view(&mut cx, store, &theme);
    cx.run_until_parked();
    if scene != "empty" {
        cx.update_window(window.into(), |root, window, cx| {
            let view = root.downcast::<NotesView>().expect("notes view");
            view.update(cx, |view, cx| {
                if scene == "todos" {
                    view.set_section(Section::Todos, window, cx);
                } else {
                    view.select(plan.clone(), true, window, cx);
                }
            });
        })
        .expect("select");
        cx.run_until_parked();
    }
    if scene == "slash" || scene == "editor" {
        let editor = editor_of(&mut cx, window);
        cx.update_window(window.into(), |_, window, cx| {
            editor.update(cx, |view, cx| {
                // Caret at the end of "Quick capture from anywhere".
                let index = view
                    .editor
                    .blocks()
                    .iter()
                    .position(|b| b.text.starts_with("Quick capture"))
                    .expect("fixture to-do");
                let len = view.editor.block(index).text.len();
                view.editor.set_caret(Pos::new(index, len));
                if scene == "slash" {
                    view.editor.enter(0);
                    view.editor
                        .backspace(diri_notes::edit::Granularity::Grapheme, 0);
                    view.replace_text_in_range(None, "/", window, cx);
                    view.replace_text_in_range(None, "h", window, cx);
                } else {
                    // Select a phrase to show selection over rich text.
                    let first = view
                        .editor
                        .blocks()
                        .iter()
                        .position(|b| b.text.starts_with("A rich"))
                        .expect("intro");
                    view.editor.set_selection(diri_notes::edit::Selection {
                        anchor: Pos::new(first, 2),
                        head: Pos::new(first, 12),
                    });
                }
                cx.notify();
            });
        })
        .expect("edit");
        cx.run_until_parked();
    }
    for _ in 0..3 {
        cx.update_window(window.into(), |_, window, _| window.refresh())
            .expect("refresh");
        cx.run_until_parked();
    }
    let screenshot = cx
        .capture_screenshot(window.into())
        .expect("capture notes screenshot");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).expect("create screenshot directory");
    }
    screenshot.save(output).expect("save notes screenshot");
}

#[gpui::test]
fn typing_through_the_input_handler_saves_markdown(cx: &mut gpui::TestAppContext) {
    let (_dir, store, plan) = fixture_store();
    let runtime = Arc::new(StoreRuntime::inert());
    let view_store = store.clone();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        NotesView::build(runtime, view_store, false, window, cx)
    });
    cx.run_until_parked();
    view.update_in(cx, |view, window, cx| view.new_note(&NewNote, window, cx));
    let editor = view.read_with(cx, |view, _| {
        view.open.as_ref().expect("open").editor.clone()
    });
    editor.update_in(cx, |view, window, cx| {
        for ch in "Standup".chars() {
            view.replace_text_in_range(None, &ch.to_string(), window, cx);
        }
        view.newline(&editor_view::Newline, window, cx);
        for ch in "[] ship **notes**".chars() {
            view.replace_text_in_range(None, &ch.to_string(), window, cx);
        }
    });
    view.update(cx, |view, cx| view.save(cx));
    let listed = store.list().expect("list");
    let standup = listed
        .iter()
        .find(|n| n.title == "Standup")
        .expect("new note saved");
    let text = std::fs::read_to_string(&standup.path).expect("read");
    assert!(
        text.ends_with("# Standup\n\n- [ ] ship **notes**\n"),
        "{text}"
    );
    assert!(listed.iter().any(|n| n.id == plan));
}

#[gpui::test]
fn abandoned_empty_notes_are_removed(cx: &mut gpui::TestAppContext) {
    let (_dir, store, plan) = fixture_store();
    let before = store.list().expect("list").len();
    let runtime = Arc::new(StoreRuntime::inert());
    let view_store = store.clone();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        NotesView::build(runtime, view_store, false, window, cx)
    });
    cx.run_until_parked();
    view.update_in(cx, |view, window, cx| {
        view.new_note(&NewNote, window, cx);
        assert_eq!(view.store.list().expect("list").len(), before + 1);
        view.select(plan.clone(), false, window, cx);
    });
    assert_eq!(store.list().expect("list").len(), before);
}

#[gpui::test]
fn outside_writes_reload_a_clean_note(cx: &mut gpui::TestAppContext) {
    let (_dir, store, plan) = fixture_store();
    let runtime = Arc::new(StoreRuntime::inert());
    let view_store = store.clone();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        NotesView::build(runtime, view_store, false, window, cx)
    });
    cx.run_until_parked();
    view.update_in(cx, |view, window, cx| {
        view.select(plan.clone(), false, window, cx)
    });
    // The CLI appends while the note is open and untouched.
    store
        .append(&plan, "- [ ] added by an agent")
        .expect("append");
    view.update(cx, |view, cx| {
        view.notes = view.store.list().expect("list");
        view.reconcile_open(cx);
    });
    let editor = view.read_with(cx, |view, _| {
        view.open.as_ref().expect("open").editor.clone()
    });
    let has = editor.read_with(cx, |view, _| {
        view.editor
            .blocks()
            .iter()
            .any(|b| b.text == "added by an agent")
    });
    assert!(has, "open note reloads outside edits");
}
