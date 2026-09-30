//! Diri Notes: a Things-calm notes window with a rich, Notion-style editor.
//!
//! Notes are Markdown files owned by `diri_notes::store`; this window lists,
//! organises, and edits them. Organisation follows Things: a few fixed places
//! (Inbox, To-dos, Pinned, Archive) plus one list per Diri project, so there
//! is no folder tree to maintain. Saving is continuous and invisible, and a
//! file watcher picks up notes written by the `dirijor note` CLI or agents.

pub(crate) mod editor_view;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use diri_notes::doc::{BlockKind, Document};
use diri_notes::edit::{Editor, Pos};
use diri_notes::markdown::FrontMatter;
use diri_notes::store::{self, Note, NoteMeta, NoteStore};
use diri_ui::{Fill, SemanticColors};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    Global, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, Render, ScrollHandle,
    SharedString, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, actions, anchored, deferred, div, point, prelude::*, px, size,
};

use crate::icons::sf_symbol;
use crate::query_editor::{self, ClipboardEdit, Edit, QueryEditor};
use crate::store::StoreRuntime;
use editor_view::{EditorEvent, NoteEditorView, accent};

pub(crate) const NOTES_CONTEXT: &str = "DiriNotes";
const SAVE_DEBOUNCE: Duration = Duration::from_millis(350);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(120);
const SIDEBAR_WIDTH: f32 = 212.0;
const LIST_WIDTH: f32 = 292.0;

actions!(
    diri_notes_window,
    [
        NewNote,
        FocusSearch,
        TogglePin,
        ToggleArchive,
        TrashNote,
        ListUp,
        ListDown,
        OpenSelected,
        CloseNotesWindow
    ]
);

pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let window = Some(NOTES_CONTEXT);
    let list = Some("DiriNotesList");
    let mut bindings = vec![
        KeyBinding::new("cmd-n", NewNote, window),
        KeyBinding::new("cmd-f", FocusSearch, window),
        KeyBinding::new("cmd-shift-p", TogglePin, window),
        KeyBinding::new("cmd-shift-a", ToggleArchive, window),
        KeyBinding::new("cmd-w", CloseNotesWindow, window),
        KeyBinding::new("up", ListUp, list),
        KeyBinding::new("down", ListDown, list),
        KeyBinding::new("enter", OpenSelected, list),
        KeyBinding::new("cmd-backspace", TrashNote, list),
    ];
    bindings.extend(editor_view::key_bindings());
    bindings
}

struct NotesWindow(WindowHandle<NotesView>);

impl Global for NotesWindow {}

/// Opens the Notes window, or brings the existing one forward.
pub(crate) fn open(runtime: Arc<StoreRuntime>, cx: &mut App) {
    if let Some(existing) = cx.try_global::<NotesWindow>().map(|w| w.0)
        && existing
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let Some(dir) = NoteStore::resolve_dir() else {
        return;
    };
    let Ok(store) = NoteStore::open(dir) else {
        return;
    };
    let material = runtime
        .store
        .read()
        .expect("session store lock poisoned")
        .preferences()
        .window_material;
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1120.0), px(740.0)),
                cx,
            ))),
            window_min_size: Some(size(px(720.0), px(420.0))),
            window_background: crate::root::window_background(material),
            app_id: Some("com.dirijor.diri.notes".to_owned()),
            titlebar: Some(TitlebarOptions {
                title: Some("Notes".into()),
                appears_transparent: cfg!(target_os = "macos"),
                traffic_light_position: cfg!(target_os = "macos")
                    .then_some(point(px(18.0), px(18.0))),
            }),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| NotesView::new(runtime, Arc::new(store), window, cx)),
    );
    if let Ok(handle) = handle {
        cx.set_global(NotesWindow(handle));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Section {
    Inbox,
    Todos,
    Pinned,
    Project(String),
    Archive,
}

impl Section {
    fn title(&self, projects: &[ProjectEntry]) -> String {
        match self {
            Self::Inbox => "Inbox".into(),
            Self::Todos => "To-dos".into(),
            Self::Pinned => "Pinned".into(),
            Self::Archive => "Archive".into(),
            Self::Project(root) => project_name(root, projects),
        }
    }

    fn contains(&self, note: &NoteMeta) -> bool {
        match self {
            Self::Inbox => !note.archived && note.project.is_none(),
            Self::Todos => !note.archived && note.todos_total > note.todos_done,
            Self::Pinned => !note.archived && note.pinned,
            Self::Archive => note.archived,
            Self::Project(root) => !note.archived && note.project.as_deref() == Some(root),
        }
    }
}

#[derive(Clone, Debug)]
struct ProjectEntry {
    root: String,
    name: String,
}

fn project_name(root: &str, projects: &[ProjectEntry]) -> String {
    projects
        .iter()
        .find(|p| p.root == root)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| {
            Path::new(root)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(root)
                .to_owned()
        })
}

struct OpenNote {
    id: String,
    front: FrontMatter,
    editor: Entity<NoteEditorView>,
    /// Markdown last written to (or read from) disk, to tell our own writes
    /// from outside edits when the watcher fires.
    saved: String,
    dirty: bool,
    _subscription: Subscription,
}

pub(crate) struct NotesView {
    runtime: Arc<StoreRuntime>,
    store: Arc<NoteStore>,
    notes: Vec<NoteMeta>,
    loaded: bool,
    section: Section,
    selected: Option<String>,
    open: Option<OpenNote>,
    search: QueryEditor,
    search_focus: FocusHandle,
    list_focus: FocusHandle,
    list_scroll: ScrollHandle,
    save_task: Task<()>,
    refresh_task: Task<()>,
    _watch_task: Task<()>,
    _watcher: Option<notify::RecommendedWatcher>,
    move_menu: bool,
    error: Option<SharedString>,
    /// Fixture palettes: (content, sidebar) for a theme id.
    theme_override: Option<(SemanticColors, SemanticColors)>,
}

impl Focusable for NotesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.list_focus.clone()
    }
}

impl NotesView {
    pub(crate) fn new(
        runtime: Arc<StoreRuntime>,
        store: Arc<NoteStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(runtime, store, true, window, cx)
    }

    /// `watch` is off only in deterministic tests, whose scheduler rejects
    /// wakeups from the file watcher's own thread.
    fn build(
        runtime: Arc<StoreRuntime>,
        store: Arc<NoteStore>,
        watch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (watcher, watch_task) = if watch {
            Self::watch(&store, cx)
        } else {
            (None, Task::ready(()))
        };
        let mut view = Self {
            runtime,
            store,
            notes: Vec::new(),
            loaded: false,
            section: Section::Inbox,
            selected: None,
            open: None,
            search: QueryEditor::default(),
            search_focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            list_scroll: ScrollHandle::new(),
            save_task: Task::ready(()),
            refresh_task: Task::ready(()),
            _watch_task: watch_task,
            _watcher: watcher,
            move_menu: false,
            error: None,
            theme_override: None,
        };
        view.load_now();
        if let Some(first) = view.visible().first().map(|n| n.id.clone()) {
            view.select(first, false, window, cx);
        }
        window.focus(&view.list_focus, cx);
        let window_handle = window.window_handle();
        cx.on_release(move |this: &mut Self, cx| {
            this.flush_save(cx);
            if cx
                .try_global::<NotesWindow>()
                .is_some_and(|open| open.0.window_id() == window_handle.window_id())
            {
                cx.remove_global::<NotesWindow>();
            }
        })
        .detach();
        view
    }

    fn colors(&self) -> SemanticColors {
        if let Some((colors, _)) = self.theme_override {
            return colors;
        }
        crate::app_theme::colors_in(&self.runtime.store.read().expect("store"))
    }

    fn sidebar_colors(&self) -> SemanticColors {
        if let Some((_, colors)) = self.theme_override {
            return colors;
        }
        crate::app_theme::sidebar_colors_in(&self.runtime.store.read().expect("store"))
    }

    fn projects(&self) -> Vec<ProjectEntry> {
        let mut entries: BTreeMap<String, ProjectEntry> = BTreeMap::new();
        {
            let store = self.runtime.store.read().expect("store");
            let mut projects: Vec<_> = store.projects().values().collect();
            projects.sort_by(|a, b| {
                a.pinned_order
                    .unwrap_or(i64::MAX)
                    .cmp(&b.pinned_order.unwrap_or(i64::MAX))
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            for project in projects {
                if project.host.is_none() {
                    entries.insert(
                        project.root.clone(),
                        ProjectEntry {
                            root: project.root.clone(),
                            name: project.name.clone(),
                        },
                    );
                }
            }
        }
        // Projects that only notes mention still get a home.
        for note in &self.notes {
            if let Some(root) = &note.project
                && !entries.contains_key(root)
            {
                let name = project_name(root, &[]);
                entries.insert(
                    root.clone(),
                    ProjectEntry {
                        root: root.clone(),
                        name,
                    },
                );
            }
        }
        let mut list: Vec<ProjectEntry> = entries.into_values().collect();
        list.sort_by_key(|p| p.name.to_lowercase());
        list
    }

    // -----------------------------------------------------------------------
    // Disk

    fn watch(
        store: &Arc<NoteStore>,
        cx: &mut Context<Self>,
    ) -> (Option<notify::RecommendedWatcher>, Task<()>) {
        use notify::{EventKind, RecursiveMode, Watcher};
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = &event
                && matches!(event.kind, EventKind::Access(_))
            {
                return;
            }
            let _ = tx.send(());
        })
        .ok()
        .and_then(|mut watcher| {
            watcher
                .watch(store.dir(), RecursiveMode::NonRecursive)
                .ok()
                .map(|()| watcher)
        });
        let task = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_some() {
                cx.background_executor().timer(WATCH_DEBOUNCE).await;
                while rx.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        (watcher, task)
    }

    fn load_now(&mut self) {
        match self.store.list() {
            Ok(notes) => {
                self.notes = notes;
                self.error = None;
            }
            Err(err) => self.error = Some(format!("Couldn't read notes: {err}").into()),
        }
        self.loaded = true;
    }

    /// Re-reads the directory off the main thread, then reconciles.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self.refresh_task = cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { store.list() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(notes) = listed {
                    this.notes = notes;
                    this.reconcile_open(cx);
                    cx.notify();
                }
            });
        });
    }

    /// An outside write to the open note (CLI append, an agent, another
    /// editor) reloads it unless the user has unsaved typing.
    fn reconcile_open(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        if open.dirty {
            return;
        }
        let Ok(path) = self.store.path_for(&open.id) else {
            return;
        };
        let Ok(source) = std::fs::read_to_string(&path) else {
            return;
        };
        if source == open.saved {
            return;
        }
        let note = store::parse_note(&source);
        let editor = Editor::new(&note.doc);
        let open = self.open.as_mut().expect("checked above");
        open.saved = source;
        open.front = note.front;
        open.editor.update(cx, |view, cx| view.reload(editor, cx));
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = &mut self.open {
            open.dirty = true;
        }
        self.save_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.save(cx));
        });
    }

    fn current_note(&self, cx: &App) -> Option<(String, Note)> {
        let open = self.open.as_ref()?;
        let doc = open.editor.read(cx).editor.document();
        Some((
            open.id.clone(),
            Note {
                front: open.front.clone(),
                doc,
            },
        ))
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some((id, note)) = self.current_note(cx) else {
            return;
        };
        let markdown = note.to_markdown();
        let Some(open) = &mut self.open else { return };
        open.dirty = false;
        if markdown == open.saved {
            return;
        }
        open.saved = markdown;
        match self.store.save(&id, &note) {
            Ok(()) => {
                self.update_meta(&id, &note);
                self.error = None;
            }
            Err(err) => self.error = Some(format!("Couldn't save: {err}").into()),
        }
        cx.notify();
    }

    /// Synchronous save for window teardown, where no task will run again.
    fn flush_save(&mut self, cx: &App) {
        let Some((id, note)) = self.current_note(cx) else {
            return;
        };
        let Some(open) = &self.open else { return };
        let empty = note.doc.title.trim().is_empty()
            && note.doc.plain_text().trim().is_empty()
            && note.doc.todo_progress().1 == 0;
        if empty {
            if let Ok(path) = self.store.path_for(&id) {
                let _ = std::fs::remove_file(path);
            }
            return;
        }
        if note.to_markdown() != open.saved {
            let _ = self.store.save(&id, &note);
        }
    }

    fn update_meta(&mut self, id: &str, note: &Note) {
        let Ok(path) = self.store.path_for(id) else {
            return;
        };
        let modified = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let meta = NoteMeta::from_note(id, path, note, modified);
        if let Some(slot) = self.notes.iter_mut().find(|n| n.id == id) {
            *slot = meta;
        } else {
            self.notes.insert(0, meta);
        }
    }

    // -----------------------------------------------------------------------
    // Selection

    fn visible(&self) -> Vec<&NoteMeta> {
        let query = self.search.text().trim().to_lowercase();
        let mut notes: Vec<&NoteMeta> = self
            .notes
            .iter()
            .filter(|note| {
                if query.is_empty() {
                    self.section.contains(note)
                } else {
                    query
                        .split_whitespace()
                        .all(|word| note.haystack.contains(word))
                }
            })
            .collect();
        // Pinned notes lead their list, like Things' "This Evening" divider.
        notes.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(b.modified_ms.cmp(&a.modified_ms))
        });
        notes
    }

    fn close_open(&mut self, cx: &mut Context<Self>) {
        self.save(cx);
        let Some(open) = self.open.take() else { return };
        // Empty notes left behind vanish, so ⌘N never litters the list.
        let doc = open.editor.read(cx).editor.document();
        if doc.title.trim().is_empty()
            && doc.plain_text().trim().is_empty()
            && doc.todo_progress().1 == 0
        {
            if let Ok(path) = self.store.path_for(&open.id) {
                let _ = std::fs::remove_file(path);
            }
            self.notes.retain(|n| n.id != open.id);
        }
    }

    fn select(
        &mut self,
        id: String,
        focus_editor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_menu = false;
        if self.open.as_ref().is_some_and(|o| o.id == id) {
            self.selected = Some(id);
            if focus_editor {
                self.focus_editor(window, cx);
            }
            cx.notify();
            return;
        }
        self.close_open(cx);
        self.selected = Some(id.clone());
        let Ok(path) = self.store.path_for(&id) else {
            return;
        };
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let note = store::parse_note(&source);
        let colors = self.colors();
        let editor = cx.new(|cx| NoteEditorView::new(Editor::new(&note.doc), colors, cx));
        let subscription =
            cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
                EditorEvent::Changed => {
                    this.schedule_save(cx);
                    cx.notify();
                }
                EditorEvent::Dismiss => {
                    this.save(cx);
                    window.focus(&this.list_focus, cx);
                    cx.notify();
                }
            });
        self.open = Some(OpenNote {
            id,
            front: note.front,
            editor,
            saved: source,
            dirty: false,
            _subscription: subscription,
        });
        if focus_editor {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = &self.open {
            let focus = open.editor.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    fn set_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.section = section;
        self.search.clear();
        self.move_menu = false;
        let keep = self
            .selected
            .as_ref()
            .and_then(|id| self.notes.iter().find(|n| &n.id == id))
            .is_some_and(|n| self.section.contains(n));
        if !keep && self.section != Section::Todos {
            match self.visible().first().map(|n| n.id.clone()) {
                Some(first) => self.select(first, false, window, cx),
                None => {
                    self.close_open(cx);
                    self.selected = None;
                }
            }
        }
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    fn new_note(&mut self, _: &NewNote, window: &mut Window, cx: &mut Context<Self>) {
        let project = match &self.section {
            Section::Project(root) => Some(root.clone()),
            _ => None,
        };
        if matches!(self.section, Section::Todos | Section::Archive) {
            self.section = Section::Inbox;
        }
        self.search.clear();
        let doc = Document::new("", Vec::new());
        match self.store.create(doc, project.as_deref()) {
            Ok((id, mut note)) => {
                if self.section == Section::Pinned {
                    note.front.set_flag(store::KEY_PINNED, true);
                    let _ = self.store.save(&id, &note);
                }
                self.update_meta(&id, &note);
                self.select(id, true, window, cx);
            }
            Err(err) => self.error = Some(format!("Couldn't create a note: {err}").into()),
        }
        cx.notify();
    }

    fn edit_front(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut FrontMatter)) {
        let Some(open) = &mut self.open else { return };
        f(&mut open.front);
        open.dirty = true;
        open.saved.clear();
        self.save(cx);
    }

    fn toggle_pin(&mut self, _: &TogglePin, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_front(cx, |front| {
            let pinned = front.flag(store::KEY_PINNED);
            front.set_flag(store::KEY_PINNED, !pinned);
        });
    }

    fn toggle_archive(&mut self, _: &ToggleArchive, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_front(cx, |front| {
            let archived = front.flag(store::KEY_ARCHIVED);
            front.set_flag(store::KEY_ARCHIVED, !archived);
        });
        self.after_leaving_section(window, cx);
    }

    fn move_to(&mut self, project: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.move_menu = false;
        self.edit_front(cx, |front| {
            front.set(store::KEY_PROJECT, project);
            front.set_flag(store::KEY_ARCHIVED, false);
        });
        self.after_leaving_section(window, cx);
    }

    /// After a note leaves the current list, select its neighbour.
    fn after_leaving_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let still = self
            .selected
            .as_ref()
            .and_then(|id| self.notes.iter().find(|n| &n.id == id))
            .is_some_and(|n| self.section.contains(n));
        if !still && self.search.text().is_empty() {
            let next = self.visible().first().map(|n| n.id.clone());
            match next {
                Some(id) => self.select(id, false, window, cx),
                None => {
                    self.close_open(cx);
                    self.selected = None;
                }
            }
        }
        cx.notify();
    }

    fn trash(&mut self, _: &TrashNote, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let visible: Vec<String> = self.visible().iter().map(|n| n.id.clone()).collect();
        let at = visible.iter().position(|v| *v == id).unwrap_or(0);
        self.save(cx);
        self.open = None;
        if self.store.trash(&id).is_ok() {
            self.notes.retain(|n| n.id != id);
        }
        self.selected = None;
        let visible: Vec<String> = self.visible().iter().map(|n| n.id.clone()).collect();
        if let Some(next) = visible
            .get(at.min(visible.len().saturating_sub(1)))
            .cloned()
        {
            self.select(next, false, window, cx);
        }
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    fn list_step(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.visible().iter().map(|n| n.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|id| ids.iter().position(|v| v == id));
        let next = match (at, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1).min(ids.len() - 1),
            (Some(i), false) => i.saturating_sub(1),
        };
        self.select(ids[next].clone(), false, window, cx);
        self.list_scroll.scroll_to_item(next);
    }

    fn on_search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" => {
                self.search.clear();
                window.focus(&self.list_focus, cx);
                cx.notify();
                cx.stop_propagation();
                return;
            }
            "enter" | "down" => {
                if let Some(first) = self.visible().first().map(|n| n.id.clone()) {
                    self.select(first, keystroke.key == "enter", window, cx);
                }
                if keystroke.key == "down" {
                    window.focus(&self.list_focus, cx);
                }
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        match query_editor::edit_for(keystroke) {
            Some(Edit::Local(edit)) => {
                self.search.apply(edit);
            }
            Some(Edit::Clipboard(ClipboardEdit::Paste)) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    self.search.insert(&text.replace('\n', " "));
                }
            }
            Some(Edit::Clipboard(ClipboardEdit::Copy)) => {
                query_editor::copy_selection(&self.search, cx)
            }
            Some(Edit::Clipboard(ClipboardEdit::Cut)) => {
                query_editor::cut_selection(&mut self.search, cx);
            }
            None => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Opens a note from the To-dos list with the caret on that to-do.
    fn open_todo(&mut self, id: &str, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.notes.iter().find(|n| n.id == id).cloned() else {
            return;
        };
        self.section = match note.project {
            Some(root) => Section::Project(root),
            None => Section::Inbox,
        };
        self.select(id.to_owned(), true, window, cx);
        if let Some(open) = &self.open {
            open.editor.update(cx, |view, cx| {
                // Editor block 0 is the title.
                let index = block + 1;
                if index < view.editor.blocks().len() {
                    let len = view.editor.block(index).text.len();
                    view.editor.set_caret(Pos::new(index, len));
                }
                cx.notify();
            });
        }
    }

    /// Ticks a to-do from the To-dos list without opening the note.
    fn tick_todo(&mut self, id: &str, block: usize, cx: &mut Context<Self>) {
        if let Some(open) = &self.open
            && open.id == id
        {
            open.editor.update(cx, |view, cx| {
                view.editor.set_checked(block + 1, true, 0);
                cx.emit(EditorEvent::Changed);
                cx.notify();
            });
            return;
        }
        let Ok(mut note) = self.store.load(id) else {
            return;
        };
        if let Some(b) = note.doc.blocks.get_mut(block)
            && matches!(b.kind, BlockKind::Todo { .. })
        {
            b.kind = BlockKind::Todo { checked: true };
            if self.store.save(id, &note).is_ok() {
                self.update_meta(id, &note);
            }
        }
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// Rendering

fn relative_date(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let secs = now.saturating_sub(ms) / 1000;
    match secs {
        0..=59 => "Just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        86_400..=172_799 => "Yesterday".into(),
        _ if secs < 7 * 86_400 => format!("{}d ago", secs / 86_400),
        _ => {
            let (_, m, d, ..) = store::civil(ms / 1000);
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            format!("{} {}", MONTHS[(m as usize).saturating_sub(1).min(11)], d)
        }
    }
}

trait AlphaExt {
    fn alpha(self, a: f32) -> Self;
}

impl AlphaExt for gpui::Rgba {
    fn alpha(self, a: f32) -> Self {
        gpui::Rgba {
            a: self.a * a,
            ..self
        }
    }
}

/// A small progress ring for a note's to-dos, drawn with two arcs of text.
fn progress_pill(done: usize, total: usize, colors: SemanticColors) -> AnyElement {
    let complete = done == total;
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .px(px(6.0))
        .h(px(18.0))
        .rounded(px(9.0))
        .bg(if complete {
            accent().alpha(0.14)
        } else {
            colors.primary.alpha(0.06)
        })
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(if complete { accent() } else { colors.secondary })
        .child(sf_symbol(
            "checklist",
            10.0,
            if complete { accent() } else { colors.secondary },
        ))
        .child(format!("{done}/{total}"))
        .into_any_element()
}

impl NotesView {
    #[allow(clippy::too_many_arguments)]
    fn sidebar_row(
        &self,
        id: &'static str,
        icon: &'static str,
        label: String,
        count: usize,
        section: Section,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.section == section && self.search.text().is_empty();
        let key = SharedString::from(format!("{id}-{label}"));
        div()
            .id(key)
            .h(px(30.0))
            .mx(px(10.0))
            .px(px(8.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .gap(px(9.0))
            .cursor_pointer()
            .when(active, |el| el.bg(Fill::selected(colors, true)))
            .when(!active, |el| {
                el.hover(|el| el.bg(Fill::hover(colors, true)))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.set_section(section.clone(), window, cx);
                }),
            )
            .child(sf_symbol(
                icon,
                14.0,
                if active { accent() } else { colors.secondary },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .font_weight(if active {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(colors.primary)
                    .child(label),
            )
            .when(count > 0, |el| {
                el.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(colors.tertiary)
                        .child(count.to_string()),
                )
            })
            .into_any_element()
    }

    fn render_sidebar(&self, projects: &[ProjectEntry], cx: &mut Context<Self>) -> AnyElement {
        let colors = self.sidebar_colors();
        let count = |section: &Section| self.notes.iter().filter(|n| section.contains(n)).count();
        let open_todos: usize = self
            .notes
            .iter()
            .filter(|n| !n.archived)
            .map(|n| n.open_todos.len())
            .sum();
        let mut column = div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(colors.sidebar_surface())
            .border_r_1()
            .border_color(colors.primary.alpha(0.06))
            .pt(px(52.0))
            .gap(px(2.0))
            .child(self.sidebar_row(
                "s",
                "doc.text",
                "Inbox".into(),
                count(&Section::Inbox),
                Section::Inbox,
                colors,
                cx,
            ))
            .child(self.sidebar_row(
                "s",
                "checklist",
                "To-dos".into(),
                open_todos,
                Section::Todos,
                colors,
                cx,
            ))
            .child(self.sidebar_row(
                "s",
                "pin",
                "Pinned".into(),
                count(&Section::Pinned),
                Section::Pinned,
                colors,
                cx,
            ));
        if !projects.is_empty() {
            column = column.child(
                div()
                    .px(px(20.0))
                    .pt(px(18.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.tertiary)
                    .child("Projects"),
            );
            let mut list = div()
                .id("notes-projects")
                .flex()
                .flex_col()
                .gap(px(2.0))
                .overflow_y_scroll()
                .flex_shrink_1();
            for project in projects {
                let section = Section::Project(project.root.clone());
                let n = count(&section);
                list = list.child(self.sidebar_row(
                    "p",
                    "folder",
                    project.name.clone(),
                    n,
                    section,
                    colors,
                    cx,
                ));
            }
            column = column.child(list);
        }
        column
            .child(div().flex_1())
            .child(self.sidebar_row(
                "s",
                "archivebox",
                "Archive".into(),
                0,
                Section::Archive,
                colors,
                cx,
            ))
            .child(
                div()
                    .id("new-note")
                    .m(px(10.0))
                    .h(px(32.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(7.0))
                    .cursor_pointer()
                    .bg(accent().alpha(0.14))
                    .hover(|el| el.bg(accent().alpha(0.22)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                            this.new_note(&NewNote, window, cx);
                        }),
                    )
                    .child(sf_symbol("plus", 13.0, accent()))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(accent())
                            .child("New Note"),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(accent().alpha(0.6))
                            .child("⌘N"),
                    ),
            )
            .into_any_element()
    }

    fn render_search(
        &self,
        colors: SemanticColors,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.search_focus.is_focused(window);
        let text = self.search.text().to_owned();
        div()
            .id("notes-search")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(Self::on_search_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.focus(&this.search_focus, cx);
                    cx.notify();
                }),
            )
            .mx(px(14.0))
            .h(px(30.0))
            .px(px(9.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .bg(colors.primary.alpha(if focused { 0.07 } else { 0.045 }))
            .border_1()
            .border_color(if focused {
                accent().alpha(0.5)
            } else {
                colors.primary.alpha(0.0)
            })
            .child(sf_symbol("magnifyingglass", 12.0, colors.tertiary))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.5))
                    .text_color(if text.is_empty() {
                        colors.tertiary
                    } else {
                        colors.primary
                    })
                    .child(if text.is_empty() && !focused {
                        "Search  ⌘F".to_owned()
                    } else {
                        text
                    })
                    .when(focused, |el| {
                        el.child(div().ml(px(1.0)).w(px(1.5)).h(px(15.0)).bg(accent()))
                    }),
            )
            .into_any_element()
    }

    fn render_list(
        &self,
        projects: &[ProjectEntry],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = self.colors();
        let searching = !self.search.text().trim().is_empty();
        let title = if searching {
            "Search".to_owned()
        } else {
            self.section.title(projects)
        };
        let notes: Vec<NoteMeta> = self.visible().into_iter().cloned().collect();
        let list_focused = self.list_focus.is_focused(window);
        let mut rows = div()
            .id("notes-list")
            .track_scroll(&self.list_scroll)
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .px(px(8.0))
            .pb(px(12.0))
            .gap(px(2.0));
        if notes.is_empty() {
            rows = rows.child(
                div()
                    .pt(px(80.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .child(sf_symbol(
                        if searching {
                            "magnifyingglass"
                        } else {
                            "doc.text"
                        },
                        22.0,
                        colors.tertiary,
                    ))
                    .child(div().text_size(px(13.0)).text_color(colors.tertiary).child(
                        if searching {
                            "No matches"
                        } else {
                            "No notes yet"
                        },
                    ))
                    .when(!searching, |el| {
                        el.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(colors.tertiary.alpha(0.8))
                                .child("⌘N to write one"),
                        )
                    }),
            );
        }
        for note in notes {
            let selected = self.selected.as_deref() == Some(note.id.as_str());
            let id = note.id.clone();
            let project = (!matches!(self.section, Section::Project(_)) || searching)
                .then(|| {
                    note.project
                        .as_ref()
                        .map(|root| project_name(root, projects))
                })
                .flatten();
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("note-{}", note.id)))
                    .px(px(12.0))
                    .py(px(10.0))
                    .rounded(px(9.0))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .cursor_pointer()
                    .when(selected, |el| {
                        el.bg(if list_focused {
                            accent().alpha(0.16)
                        } else {
                            colors.primary.alpha(0.07)
                        })
                    })
                    .when(!selected, |el| {
                        el.hover(|el| el.bg(colors.primary.alpha(0.035)))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.select(id.clone(), event.click_count >= 2, window, cx);
                            if event.click_count < 2 {
                                window.focus(&this.list_focus, cx);
                            }
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .when(note.pinned, |el| {
                                el.child(sf_symbol("pin.fill", 10.0, accent()))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if note.title.trim().is_empty() {
                                        colors.tertiary
                                    } else {
                                        colors.primary
                                    })
                                    .child(note.display_title().to_owned()),
                            ),
                    )
                    .when(!note.snippet.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(12.5))
                                .line_height(px(17.0))
                                .text_color(colors.secondary)
                                .line_clamp(2)
                                .child(note.snippet.clone()),
                        )
                    })
                    .child(
                        div()
                            .pt(px(3.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(px(11.0))
                            .text_color(colors.tertiary)
                            .child(relative_date(note.modified_ms))
                            .when_some(project, |el, name| {
                                el.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.0))
                                        .child(sf_symbol("folder", 10.0, colors.tertiary))
                                        .child(name),
                                )
                            })
                            .child(div().flex_1())
                            .when(note.todos_total > 0, |el| {
                                el.child(progress_pill(note.todos_done, note.todos_total, colors))
                            }),
                    ),
            );
        }
        div()
            .id("notes-list-column")
            .key_context("DiriNotesList")
            .track_focus(&self.list_focus)
            .on_action(
                cx.listener(|this, _: &ListUp, window, cx| this.list_step(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ListDown, window, cx| this.list_step(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenSelected, window, cx| {
                this.focus_editor(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(Self::trash))
            .w(px(LIST_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(colors.primary.alpha(0.06))
            .bg(colors.work_surface())
            .child(
                div()
                    .h(px(52.0))
                    .px(px(22.0))
                    .flex()
                    .items_end()
                    .pb(px(10.0))
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(17.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(colors.primary)
                            .child(title),
                    ),
            )
            .child(self.render_search(colors, window, cx))
            .child(div().h(px(10.0)))
            .child(rows)
            .into_any_element()
    }

    fn render_todos(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors();
        let projects = self.projects();
        let mut groups: Vec<&NoteMeta> = self
            .notes
            .iter()
            .filter(|n| !n.archived && !n.open_todos.is_empty())
            .collect();
        groups.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(b.modified_ms.cmp(&a.modified_ms))
        });
        let mut column = div()
            .w_full()
            .max_w(px(editor_view::MEASURE))
            .flex()
            .flex_col()
            .gap(px(26.0));
        if groups.is_empty() {
            column = column.child(
                div()
                    .pt(px(100.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .child(sf_symbol("checkmark.circle", 30.0, accent()))
                    .child(
                        div()
                            .text_size(px(15.0))
                            .text_color(colors.secondary)
                            .child("All done"),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(colors.tertiary)
                            .child("Type [] in any note to add a to-do"),
                    ),
            );
        }
        for note in groups {
            let mut group = div().flex().flex_col().gap(px(2.0)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .pb(px(6.0))
                    .mb(px(4.0))
                    .border_b_1()
                    .border_color(colors.primary.alpha(0.07))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.primary)
                            .child(note.display_title().to_owned()),
                    )
                    .when_some(note.project.as_ref(), |el, root| {
                        el.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(colors.tertiary)
                                .child(project_name(root, &projects)),
                        )
                    }),
            );
            for (block, text) in &note.open_todos {
                let block = *block;
                let open_id = note.id.clone();
                let tick_id = note.id.clone();
                group = group.child(
                    div()
                        .id(SharedString::from(format!("todo-{}-{block}", note.id)))
                        .flex()
                        .items_start()
                        .gap(px(10.0))
                        .py(px(4.0))
                        .px(px(6.0))
                        .rounded(px(7.0))
                        .hover(|el| el.bg(colors.primary.alpha(0.035)))
                        .child(
                            div()
                                .id(SharedString::from(format!("tick-{}-{block}", note.id)))
                                .mt(px(3.0))
                                .size(px(16.0))
                                .flex_none()
                                .rounded(px(5.0))
                                .border(px(1.5))
                                .border_color(colors.primary.alpha(0.32))
                                .cursor_pointer()
                                .hover(|el| el.border_color(accent()).bg(accent().alpha(0.1)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.tick_todo(&tick_id, block, cx);
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .cursor_pointer()
                                .text_size(px(14.5))
                                .line_height(px(22.0))
                                .text_color(colors.primary)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                        this.open_todo(&open_id, block, window, cx);
                                    }),
                                )
                                .child(text.clone()),
                        ),
                );
            }
            column = column.child(group);
        }
        div()
            .id("todos")
            .size_full()
            .overflow_y_scroll()
            .bg(colors.work_surface())
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .px(px(48.0))
                    .pt(px(56.0))
                    .pb(px(120.0))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(editor_view::MEASURE))
                            .pb(px(26.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(sf_symbol("checklist", 22.0, accent()))
                            .child(
                                div()
                                    .text_size(px(28.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(colors.primary)
                                    .child("To-dos"),
                            ),
                    )
                    .child(column),
            )
            .into_any_element()
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        icon: &'static str,
        active: bool,
        colors: SemanticColors,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .size(px(28.0))
            .rounded(px(7.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|el| el.bg(colors.primary.alpha(0.07)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    on_click(this, window, cx);
                }),
            )
            .child(sf_symbol(
                icon,
                14.0,
                if active { accent() } else { colors.secondary },
            ))
            .into_any_element()
    }

    fn render_detail(&self, projects: &[ProjectEntry], cx: &mut Context<Self>) -> AnyElement {
        let colors = self.colors();
        let Some(open) = &self.open else {
            return div()
                .size_full()
                .bg(colors.work_surface())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(10.0))
                        .child(sf_symbol("square.and.pencil", 28.0, colors.tertiary))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(colors.tertiary)
                                .child("Select a note, or press ⌘N"),
                        ),
                )
                .into_any_element();
        };
        open.editor.update(cx, |view, _| view.set_colors(colors));
        let pinned = open.front.flag(store::KEY_PINNED);
        let archived = open.front.flag(store::KEY_ARCHIVED);
        let project = open.front.get(store::KEY_PROJECT).map(str::to_owned);
        let place = project
            .as_ref()
            .map(|root| project_name(root, projects))
            .unwrap_or_else(|| "Inbox".to_owned());
        let dirty = open.dirty;
        let toolbar = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(46.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                div()
                    .id("move-note")
                    .h(px(26.0))
                    .px(px(9.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(colors.primary.alpha(0.07)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.move_menu = !this.move_menu;
                            cx.notify();
                        }),
                    )
                    .child(sf_symbol(
                        if project.is_some() {
                            "folder"
                        } else {
                            "doc.text"
                        },
                        12.0,
                        colors.secondary,
                    ))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(colors.secondary)
                            .child(place),
                    )
                    .child(sf_symbol("chevron.down", 9.0, colors.tertiary)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .mr(px(6.0))
                    .text_size(px(11.0))
                    .text_color(colors.tertiary)
                    .child(if dirty { "Saving…" } else { "" }),
            )
            .child(self.toolbar_button(
                "pin-note",
                if pinned { "pin.fill" } else { "pin" },
                pinned,
                colors,
                |this, window, cx| this.toggle_pin(&TogglePin, window, cx),
                cx,
            ))
            .child(self.toolbar_button(
                "archive-note",
                if archived {
                    "tray.and.arrow.up.fill"
                } else {
                    "archivebox"
                },
                false,
                colors,
                |this, window, cx| this.toggle_archive(&ToggleArchive, window, cx),
                cx,
            ))
            .child(self.toolbar_button(
                "trash-note",
                "trash",
                false,
                colors,
                |this, window, cx| this.trash(&TrashNote, window, cx),
                cx,
            ));
        let move_menu = self.move_menu.then(|| {
            let mut menu = div()
                .w(px(220.0))
                .py(px(6.0))
                .rounded(px(12.0))
                .bg(colors.floating_fill())
                .border_1()
                .border_color(colors.primary.alpha(0.08))
                .shadow_lg()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px(px(14.0))
                        .pt(px(4.0))
                        .pb(px(6.0))
                        .text_size(px(11.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.tertiary)
                        .child("Move to"),
                );
            let mut targets: Vec<(Option<String>, String, &'static str)> =
                vec![(None, "Inbox".into(), "doc.text")];
            targets.extend(
                projects
                    .iter()
                    .map(|p| (Some(p.root.clone()), p.name.clone(), "folder")),
            );
            for (i, (root, name, icon)) in targets.into_iter().enumerate() {
                let current = root == project;
                menu = menu.child(
                    div()
                        .id(("move-target", i))
                        .mx(px(6.0))
                        .px(px(8.0))
                        .h(px(30.0))
                        .rounded(px(7.0))
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .cursor_pointer()
                        .hover(|el| el.bg(accent().alpha(0.14)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.move_to(root.clone(), window, cx);
                            }),
                        )
                        .child(sf_symbol(icon, 12.0, colors.secondary))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_size(px(13.0))
                                .text_color(colors.primary)
                                .child(name),
                        )
                        .when(current, |el| {
                            el.child(sf_symbol("checkmark", 11.0, accent()))
                        }),
                );
            }
            deferred(
                anchored()
                    .position(point(px(0.0), px(0.0)))
                    .child(div().pt(px(40.0)).pl(px(16.0)).child(menu)),
            )
            .with_priority(1)
        });
        div()
            .relative()
            .size_full()
            .bg(colors.work_surface())
            .child(open.editor.clone())
            .child(toolbar)
            .children(move_menu.map(|menu| div().absolute().top_0().left_0().child(menu)))
            .into_any_element()
    }
}

impl Render for NotesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::app_theme::follow(&self.runtime.store.read().expect("store"), window, cx);
        let colors = self.colors();
        let projects = self.projects();
        let main = if self.section == Section::Todos && self.search.text().trim().is_empty() {
            self.render_todos(cx)
        } else {
            div()
                .flex()
                .flex_row()
                .size_full()
                .child(self.render_list(&projects, window, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.render_detail(&projects, cx)),
                )
                .into_any_element()
        };
        let error = self.error.clone();
        div()
            .key_context(NOTES_CONTEXT)
            .size_full()
            .flex()
            .flex_row()
            .bg(colors.window_fill())
            .font_family(crate::fonts::ui_family())
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(&this.search_focus, cx);
                this.search.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(Self::toggle_pin))
            .on_action(cx.listener(Self::toggle_archive))
            .on_action(cx.listener(|this, _: &CloseNotesWindow, window, cx| {
                this.close_open(cx);
                window.remove_window();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    if this.move_menu {
                        this.move_menu = false;
                        cx.notify();
                    }
                }),
            )
            .child(self.render_sidebar(&projects, cx))
            .child(div().flex_1().min_w_0().h_full().child(main))
            .when_some(error, |el, error| {
                el.child(
                    div()
                        .absolute()
                        .bottom(px(14.0))
                        .right(px(14.0))
                        .px(px(12.0))
                        .py(px(7.0))
                        .rounded(px(8.0))
                        .bg(diri_ui::Ink::DANGER.alpha(0.9))
                        .text_color(gpui::white())
                        .text_size(px(12.0))
                        .child(error),
                )
            })
    }
}
