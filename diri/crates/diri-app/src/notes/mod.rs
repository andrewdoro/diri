//! Diri Notes inside the main window.
//!
//! A note is a Session whose kind is `note`: it sits in the sidebar, sorts,
//! pins, archives, and nests exactly like an agent or a terminal, and a
//! session started from it is its child. When one is selected, RootView shows
//! this pane where the terminal would be. The pane owns the open note's file
//! (via `diri_notes::store`), saves continuously, keeps the Session's title in
//! step with the note's, and reloads writes made by the CLI or agents.

pub(crate) mod editor_view;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod todos;
pub(crate) mod work_item;
#[cfg(test)]
pub(crate) mod work_item_tests;

use std::sync::Arc;
use std::time::Duration;

use diri_notes::edit::Editor;
use diri_notes::markdown::FrontMatter;
use diri_notes::mention::{self as mentions, Candidate, MentionTarget};
use diri_notes::store::{self, Note, NoteStore};
use diri_proto::SessionId;
use diri_ui::SemanticColors;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, KeyBinding, Render, SharedString, Subscription,
    Task, Window, div, prelude::*, px,
};

use crate::store::StoreRuntime;
use editor_view::{EditorEvent, MentionDirectory, MentionEntry, NoteEditorView};

const SAVE_DEBOUNCE: Duration = Duration::from_millis(350);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(120);

pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    editor_view::key_bindings()
}

struct OpenNote {
    session: SessionId,
    id: String,
    front: FrontMatter,
    editor: Entity<NoteEditorView>,
    /// Markdown last written to (or read from) disk, to tell our own writes
    /// from outside edits when the watcher fires.
    saved: String,
    dirty: bool,
    /// The title last pushed to the Session, so renames go out once.
    synced_title: String,
    _subscription: Subscription,
}

enum PaneState {
    Empty,
    Open(OpenNote),
    Missing { session: SessionId, id: String },
}

pub(crate) struct NotePane {
    runtime: Arc<StoreRuntime>,
    store: Option<Arc<NoteStore>>,
    state: PaneState,
    focus: FocusHandle,
    focus_on_show: bool,
    save_task: Task<()>,
    _watch_task: Task<()>,
    _watcher: Option<notify::RecommendedWatcher>,
    /// Keeps mention chips' session status and the `@` menu live.
    _sessions_task: Task<()>,
    error: Option<SharedString>,
    /// Fixture palette; live panes follow the store's theme.
    colors_override: Option<SemanticColors>,
}

impl Focusable for NotePane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.state {
            PaneState::Open(open) => open.editor.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        }
    }
}

fn editor_view_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub(crate) enum NotePaneEvent {
    /// Escape with nothing left to dismiss in the editor.
    Dismiss,
    /// A mention chip was clicked: show that session (an agent, a
    /// terminal, or another note).
    Reveal(SessionId),
}

impl gpui::EventEmitter<NotePaneEvent> for NotePane {}

impl NotePane {
    pub(crate) fn new(runtime: Arc<StoreRuntime>, cx: &mut Context<Self>) -> Self {
        let store = NoteStore::resolve_dir()
            .and_then(|dir| NoteStore::open(dir).ok())
            .map(Arc::new);
        Self::with_store(runtime, store, true, cx)
    }

    /// `watch` is off only in deterministic tests, whose scheduler rejects
    /// wakeups from the file watcher's own thread.
    pub(crate) fn with_store(
        runtime: Arc<StoreRuntime>,
        store: Option<Arc<NoteStore>>,
        watch: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let (watcher, watch_task) = match (&store, watch) {
            (Some(store), true) => Self::watch(store, cx),
            _ => (None, Task::ready(())),
        };
        let sessions_task = if watch {
            Self::follow_sessions(&runtime, cx)
        } else {
            Task::ready(())
        };
        Self {
            runtime,
            store,
            state: PaneState::Empty,
            focus: cx.focus_handle(),
            focus_on_show: false,
            save_task: Task::ready(()),
            _watch_task: watch_task,
            _watcher: watcher,
            _sessions_task: sessions_task,
            error: None,
            colors_override: None,
        }
    }

    fn colors(&self) -> SemanticColors {
        self.colors_override.unwrap_or_else(|| {
            crate::app_theme::colors_in(&self.runtime.store.read().expect("store"))
        })
    }

    #[cfg(test)]
    pub(crate) fn editor_for_test(&self) -> Option<Entity<NoteEditorView>> {
        match &self.state {
            PaneState::Open(open) => Some(open.editor.clone()),
            _ => None,
        }
    }

    /// Focus the editor the next time a note is shown (a new note, a click).
    pub(crate) fn request_focus(&mut self) {
        self.focus_on_show = true;
    }

    /// Shows `note_id` for `session`; switching away saves the previous one.
    pub(crate) fn show(
        &mut self,
        session: &SessionId,
        note_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same = match &self.state {
            PaneState::Open(open) => open.session == *session && open.id == note_id,
            PaneState::Missing { session: s, id } => s == session && id == note_id,
            PaneState::Empty => false,
        };
        if !same {
            self.save(cx);
            self.load(session, note_id, window, cx);
        }
        // Arrived from a session's header: show the to-do it works on.
        let reveal = self
            .runtime
            .store
            .write()
            .expect("store")
            .take_note_reveal(session);
        if let (Some(child), PaneState::Open(open)) = (reveal, &self.state) {
            let editor = open.editor.clone();
            if editor.update(cx, |view, cx| view.reveal_session(&child.0, cx)) {
                self.focus_on_show = true;
            }
        }
        if std::mem::take(&mut self.focus_on_show) {
            let handle = self.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Caret to the end of a note block (index in the file, title excluded)
    /// and focus the editor, as a jump from the To-dos page.
    pub(crate) fn reveal_block(
        &mut self,
        block: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PaneState::Open(open) = &self.state else {
            return;
        };
        open.editor.update(cx, |view, cx| {
            let index = (block + 1).min(view.editor.blocks().len() - 1);
            let len = view.editor.block(index).text.len();
            view.editor
                .set_caret(diri_notes::edit::Pos::new(index, len));
            cx.notify();
        });
        let handle = self.focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn load(
        &mut self,
        session: &SessionId,
        note_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = self
            .store
            .as_ref()
            .and_then(|store| store.path_for(note_id).ok())
            .and_then(|path| std::fs::read_to_string(path).ok());
        let Some(source) = source else {
            self.state = PaneState::Missing {
                session: session.clone(),
                id: note_id.to_owned(),
            };
            cx.notify();
            return;
        };
        let note = store::parse_note(&source);
        let colors = self.colors();
        let editor = cx.new(|cx| {
            let mut view = NoteEditorView::new(Editor::new(&note.doc), colors, cx);
            view.fold_started_work();
            view
        });
        let subscription = cx.subscribe_in(&editor, window, |this, _, event, _, cx| match event {
            EditorEvent::Changed => this.schedule_save(cx),
            EditorEvent::Dismiss => {
                this.save(cx);
                cx.emit(NotePaneEvent::Dismiss);
            }
            EditorEvent::OpenMention(target) => this.open_mention(target, cx),
            EditorEvent::Work(request) => this.on_work(request, cx),
        });
        let synced_title = self
            .runtime
            .store
            .read()
            .expect("store")
            .sessions()
            .get(session)
            .map(|record| record.title.clone())
            .unwrap_or_default();
        self.state = PaneState::Open(OpenNote {
            session: session.clone(),
            id: note_id.to_owned(),
            front: note.front,
            editor,
            saved: source,
            dirty: false,
            synced_title,
            _subscription: subscription,
        });
        self.push_mentions(cx);
        self.push_work(cx);
        cx.notify();
    }

    fn follow_sessions(runtime: &Arc<StoreRuntime>, cx: &mut Context<Self>) -> Task<()> {
        let mut changes = runtime.changes();
        cx.spawn(async move |this, cx| {
            loop {
                match changes.recv().await {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if this
                            .update(cx, |this, cx| {
                                this.push_mentions(cx);
                                this.push_work(cx);
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }

    /// Everything the open note can mention, rebuilt from the session store.
    pub(crate) fn mention_directory(&self) -> MentionDirectory {
        let open = match &self.state {
            PaneState::Open(open) => Some(open.session.clone()),
            _ => None,
        };
        let store = self.runtime.store.read().expect("store");
        let entries = mention_entries(&store, open.as_ref());
        MentionDirectory { entries }
    }

    /// Hands the open editor a fresh directory; a no-op when nothing changed.
    pub(crate) fn push_mentions(&mut self, cx: &mut Context<Self>) {
        let PaneState::Open(open) = &self.state else {
            return;
        };
        let editor = open.editor.clone();
        let directory = self.mention_directory();
        editor.update(cx, |view, cx| view.set_mentions(directory, cx));
    }

    fn open_mention(&mut self, target: &MentionTarget, cx: &mut Context<Self>) {
        let session = {
            let store = self.runtime.store.read().expect("store");
            match target {
                MentionTarget::Session(id) => {
                    let id = SessionId::new(id.clone());
                    store.sessions().contains_key(&id).then_some(id)
                }
                MentionTarget::Note(note) => store
                    .sessions()
                    .values()
                    .find(|s| s.is_note() && s.note_id.as_deref() == Some(note))
                    .map(|s| s.id.clone()),
            }
        };
        if let Some(session) = session {
            self.save(cx);
            cx.emit(NotePaneEvent::Reveal(session));
        }
    }

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
                if this.update(cx, |this, cx| this.reconcile(cx)).is_err() {
                    break;
                }
            }
        });
        (watcher, task)
    }

    /// An outside write to the open note (CLI append, an agent, another
    /// editor). A clean note reloads; with unsaved typing the write is merged
    /// into the editor as one undo step, so neither side is lost.
    pub(crate) fn reconcile(&mut self, cx: &mut Context<Self>) {
        let PaneState::Open(open) = &self.state else {
            return;
        };
        let Some(path) = self.store.as_ref().and_then(|s| s.path_for(&open.id).ok()) else {
            return;
        };
        let Ok(source) = std::fs::read_to_string(&path) else {
            return;
        };
        if source == open.saved {
            return;
        }
        self.absorb_outside(source, cx);
    }

    /// Takes the file's current `source` into the open note: a reload when
    /// there is no unsaved typing, else a three-way merge against what the
    /// editor last loaded or saved.
    fn absorb_outside(&mut self, source: String, cx: &mut Context<Self>) {
        let PaneState::Open(open) = &mut self.state else {
            return;
        };
        let theirs = store::parse_note(&source);
        if open.dirty {
            let base = store::parse_note(&open.saved).doc;
            open.editor.update(cx, |view, cx| {
                let merged = diri_notes::merge::merge3(&base, &view.editor.document(), &theirs.doc);
                view.editor.absorb(merged, diri_notes::history::now_ms());
                cx.notify();
            });
        } else {
            let editor = Editor::new(&theirs.doc);
            open.editor.update(cx, |view, cx| view.reload(editor, cx));
        }
        open.saved = source;
        open.front = theirs.front;
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if let PaneState::Open(open) = &mut self.state {
            open.dirty = true;
        }
        self.save_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.save(cx));
        });
    }

    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        // Save only over the version this editor knows; an outside write in
        // between is merged in first, then saved over. Bounded: a file that
        // keeps changing under us is retried on the next save.
        let mut note = None;
        for _ in 0..3 {
            let PaneState::Open(open) = &mut self.state else {
                return;
            };
            let current = Note {
                front: open.front.clone(),
                doc: open.editor.read(cx).editor.document(),
            };
            let markdown = current.to_markdown();
            if markdown == open.saved {
                open.dirty = false;
                note = Some(current);
                break;
            }
            match store.save_if_unchanged(&open.id, &current, &open.saved) {
                Ok(store::SaveOutcome::Saved { source }) => {
                    open.saved = source;
                    open.dirty = false;
                    self.error = None;
                    note = Some(current);
                    break;
                }
                Ok(store::SaveOutcome::Conflict {
                    current: Some(outside),
                }) => {
                    open.dirty = true;
                    self.absorb_outside(outside, cx);
                }
                Ok(store::SaveOutcome::Conflict { current: None }) => {
                    self.error = Some(
                        "This note's file was moved or deleted outside Diri; your text is still here."
                            .into(),
                    );
                    return;
                }
                Err(err) => {
                    self.error = Some(format!("Couldn't save this note: {err}").into());
                    return;
                }
            }
        }
        let Some(note) = note else {
            return;
        };
        // The sidebar row is the Session's title; keep it the note's title.
        let PaneState::Open(open) = &mut self.state else {
            return;
        };
        let title = note.doc.title.trim();
        let title = if title.is_empty() { "Untitled" } else { title };
        if title != open.synced_title {
            open.synced_title = title.to_owned();
            self.runtime
                .store
                .write()
                .expect("store")
                .rename(open.session.clone(), title);
        }
        cx.notify();
    }
}

impl Render for NotePane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.colors();
        let root = div()
            .relative()
            .size_full()
            .bg(colors.work_surface_nested())
            .font_family(crate::fonts::ui_family());
        let root = match &self.state {
            PaneState::Open(open) => {
                open.editor.update(cx, |view, _| view.set_colors(colors));
                root.child(open.editor.clone())
            }
            PaneState::Missing { .. } => root.track_focus(&self.focus).child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(colors.secondary)
                            .child("This note's file is gone"),
                    )
                    .child(div().text_size(px(12.0)).text_color(colors.tertiary).child(
                        "It may have been deleted outside Diri. Archive this tab to tidy up.",
                    )),
            ),
            PaneState::Empty => root.track_focus(&self.focus),
        };
        root.when_some(self.error.clone(), |el, error| {
            el.child(
                div()
                    .absolute()
                    .bottom(px(14.0))
                    .right(px(14.0))
                    .px(px(12.0))
                    .py(px(7.0))
                    .rounded(px(8.0))
                    .bg(diri_ui::Ink::DANGER)
                    .text_color(gpui::white())
                    .text_size(px(12.0))
                    .child(error),
            )
        })
    }
}

/// Live sessions first (most recently active), then notes, each as the
/// `@` menu offers it. Notes are Sessions too; they are mentioned by their
/// file id so the link survives the Session being archived and restored.
fn mention_entries(
    store: &crate::store::SessionStore,
    open: Option<&SessionId>,
) -> Vec<MentionEntry> {
    let mut records: Vec<_> = store
        .sessions()
        .values()
        .filter(|s| !s.is_archived() && Some(&s.id) != open)
        .collect();
    records.sort_by(|a, b| b.updated_at.0.total_cmp(&a.updated_at.0));
    let project_of = |record: &diri_proto::SessionRecord| {
        store
            .projects()
            .get(&record.project_id)
            .map(|p| p.name.clone())
            .unwrap_or_default()
    };
    let (notes, sessions): (Vec<_>, Vec<_>) = records.into_iter().partition(|s| s.is_note());
    let mut entries: Vec<MentionEntry> = sessions
        .into_iter()
        .map(|session| {
            let kind = session.effective_kind();
            let agent = crate::notifications::display_name(kind, store.agent_descriptor(kind));
            let title = crate::switcher::display_title_str(session);
            let project = project_of(session);
            let detail = match (&session.host, project.is_empty()) {
                (Some(host), false) => format!("{project} · {host}"),
                (Some(host), true) => host.clone(),
                (None, false) => project.clone(),
                (None, true) => {
                    crate::quick_open::home_relative(std::path::Path::new(&session.cwd))
                }
            };
            MentionEntry {
                candidate: Candidate {
                    target: MentionTarget::Session(session.id.0.clone()),
                    label: mentions::session_label(agent, title),
                    keywords: format!(
                        "{agent} {project} {}",
                        session.git_branch.as_deref().unwrap_or("")
                    ),
                },
                agent: Some(crate::session_presentation::ui_agent_kind(kind)),
                status: Some(crate::session_presentation::status_state(session, false)),
                detail: detail.into(),
            }
        })
        .collect();
    entries.extend(notes.into_iter().filter_map(|note| {
        let id = note.note_id.clone()?;
        let project = project_of(note);
        Some(MentionEntry {
            candidate: Candidate {
                target: MentionTarget::Note(id),
                label: mentions::note_label(&note.title),
                keywords: format!("note {project}"),
            },
            agent: None,
            status: None,
            detail: project.into(),
        })
    }));
    entries
}
