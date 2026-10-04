// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! The right panel's API surface: build an HTTP request, send it, read the
//! answer. A port of Ely's devtools (`ApiRequestBuilder`, `QueryParamsEditor`
//! and `KeyValueInput`, `HeadersTable`, `ResponseViewer`, `EnvironmentSelector`,
//! `CollectionTree`) onto diri's tokens, with a real sender behind it.
//!
//! - `model`: requests, pairs, environments, collections, history (plain data)
//! - `json`: order-preserving pretty JSON with fold ranges
//! - `http`: the hardened `curl` sender, off the main thread
//! - `storage`: the per-project owner-only store
//! - `view`: rendering
//!
//! Agents open a prefilled request here with the `open_api_request` MCP tool
//! (`WorkbenchInspector::open_api_request`). Only a `GET` they mark
//! `autoSend` goes out by itself; anything else waits for Send.

pub mod http;
pub mod json;
pub mod model;
pub mod storage;
mod view;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use diri_ui::SemanticColors;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, KeyDownEvent, Task,
    UniformListScrollHandle, WeakEntity, Window,
};

use crate::query_editor::{self, ClipboardEdit, Edit, QueryEditor};
use http::{ApiResponse, HttpError};
use json::PrettyJson;
use model::{ApiRequest, Auth, BodyKind, Environment, HistoryEntry, Method, Pair, Saved, Variable};
use storage::ApiProject;

/// How long the store waits for typing to settle before writing.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// One project's collections, environments and history, shared by every API
/// tab open on that project and written back to its store file.
pub struct ApiLibrary {
    path: PathBuf,
    pub project: ApiProject,
    /// Why the store could not be read or written, for the panel to say.
    pub error: Option<String>,
    save_task: Option<Task<()>>,
}

impl ApiLibrary {
    fn load(path: PathBuf) -> Self {
        let (mut project, error) = match storage::load(&path) {
            Ok(project) => (project, None),
            Err(error) => (
                ApiProject::default(),
                Some(format!("Couldn’t read saved requests: {error}")),
            ),
        };
        for item in &mut project.collections {
            normalize_saved(item);
        }
        Self {
            path,
            project,
            error,
            save_task: None,
        }
    }

    pub fn active_environment(&self) -> Option<&Environment> {
        let id = self.project.active_environment.as_ref()?;
        self.project
            .environments
            .iter()
            .find(|environment| &environment.id == id)
    }

    pub fn environment_mut(&mut self, id: &str) -> Option<&mut Environment> {
        self.project
            .environments
            .iter_mut()
            .find(|environment| environment.id == id)
    }

    /// Writes the store once edits settle.
    pub fn changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let Ok((path, project)) =
                this.read_with(cx, |this, _| (this.path.clone(), this.project.clone()))
            else {
                return;
            };
            let result = cx
                .background_spawn(async move { storage::save(&path, &project) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let error = result
                    .err()
                    .map(|error| format!("Couldn’t save requests: {error}"));
                if this.error != error {
                    this.error = error;
                    cx.notify();
                }
            });
        }));
    }

    /// Writes the store now, on this thread. Tests and shutdown paths only.
    #[cfg(test)]
    pub fn save_now(&mut self) -> std::io::Result<()> {
        self.save_task = None;
        storage::save(&self.path, &self.project)
    }

    /// Merges variables an agent sent into the environment of that name
    /// (created when missing), and makes it the active one.
    pub fn merge_environment(&mut self, name: &str, variables: Vec<Variable>) {
        let id = match self
            .project
            .environments
            .iter()
            .find(|environment| environment.name == name)
        {
            Some(environment) => environment.id.clone(),
            None => {
                let id = model::fresh_id("env");
                self.project.environments.push(Environment {
                    id: id.clone(),
                    name: name.to_owned(),
                    variables: Vec::new(),
                });
                id
            }
        };
        if let Some(environment) = self.environment_mut(&id) {
            environment.merge(variables);
        }
        self.project.active_environment = Some(id);
    }
}

fn normalize_saved(item: &mut Saved) {
    match item {
        Saved::Folder { items, .. } => items.iter_mut().for_each(normalize_saved),
        Saved::Request { request } => request.normalize(),
    }
}

thread_local! {
    /// Open libraries by store file, so two tabs on one project share edits.
    static LIBRARIES: RefCell<HashMap<PathBuf, WeakEntity<ApiLibrary>>> = RefCell::new(HashMap::new());
}

/// The library for `project` stored under `root`, shared with any tab that
/// already has it open.
pub fn library_for(root: &Path, project: &str, cx: &mut App) -> Entity<ApiLibrary> {
    let path = storage::project_file(root, project);
    if let Some(library) =
        LIBRARIES.with(|libraries| libraries.borrow().get(&path).and_then(WeakEntity::upgrade))
    {
        return library;
    }
    let library = cx.new(|_| ApiLibrary::load(path.clone()));
    LIBRARIES.with(|libraries| {
        let mut libraries = libraries.borrow_mut();
        libraries.retain(|_, library| library.upgrade().is_some());
        libraries.insert(path, library.downgrade());
    });
    library
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Request,
    Collections,
    History,
    Environments,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestTab {
    Params,
    Headers,
    Body,
    Auth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseTab {
    Body,
    Headers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    Name,
    Value,
}

/// A text field of the surface. Rows are addressed by index into the table
/// they belong to; environment fields address the environment being edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Url,
    Name,
    Param(usize, Column),
    Header(usize, Column),
    Form(usize, Column),
    Body,
    BearerToken,
    BasicUser,
    BasicPassword,
    EnvName,
    Variable(usize, Column),
    FolderName,
}

impl Field {
    /// A field whose text is a secret: drawn as dots until focused.
    fn masked(self) -> bool {
        matches!(self, Field::BearerToken | Field::BasicPassword)
    }
}

/// A response as shown: the bytes, and the JSON and plain-line layouts the
/// viewer draws from, built once off the hot render path.
pub struct ResponseView {
    pub response: ApiResponse,
    pub pretty: Option<Arc<PrettyJson>>,
    pub raw_lines: Arc<Vec<String>>,
}

impl ResponseView {
    pub fn new(response: ApiResponse) -> Self {
        let text = response.body_text();
        let pretty = PrettyJson::parse(&text).map(Arc::new);
        let raw_lines = Arc::new(text.lines().map(str::to_owned).collect());
        Self {
            response,
            pretty,
            raw_lines,
        }
    }
}

pub enum Outcome {
    Response(ResponseView),
    Failed(String),
}

struct Sending {
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    _task: Task<()>,
}

pub struct ApiClient {
    tokio: tokio::runtime::Handle,
    colors: SemanticColors,
    focus: FocusHandle,
    library: Entity<ApiLibrary>,
    pub(crate) draft: ApiRequest,
    /// The draft has not been touched since it was opened or loaded, so an
    /// agent's next request may replace it.
    pristine: bool,
    pub(crate) mode: Mode,
    request_tab: RequestTab,
    response_tab: ResponseTab,
    response_raw: bool,
    field: Option<(Field, QueryEditor)>,
    method_menu_open: bool,
    sending: Option<Sending>,
    pub(crate) outcome: Option<Outcome>,
    folded: HashSet<usize>,
    body_scroll: UniformListScrollHandle,
    tree_open: HashSet<String>,
    tree_selected: Option<String>,
    revealed: HashSet<(String, usize)>,
    editing_environment: Option<String>,
    send_generation: u64,
}

impl Focusable for ApiClient {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ApiClient {
    pub fn new(
        tokio: tokio::runtime::Handle,
        library: Entity<ApiLibrary>,
        colors: SemanticColors,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        let editing_environment = library.read(cx).project.active_environment.clone();
        Self {
            tokio,
            colors,
            focus: cx.focus_handle(),
            library,
            draft: ApiRequest::new(model::fresh_id("req"), Method::Get, ""),
            pristine: true,
            mode: Mode::Request,
            request_tab: RequestTab::Params,
            response_tab: ResponseTab::Body,
            response_raw: false,
            field: None,
            method_menu_open: false,
            sending: None,
            outcome: None,
            folded: HashSet::new(),
            body_scroll: UniformListScrollHandle::new(),
            tree_open: HashSet::new(),
            tree_selected: None,
            revealed: HashSet::new(),
            editing_environment,
            send_generation: 0,
        }
    }

    pub fn set_colors(&mut self, colors: SemanticColors, cx: &mut Context<Self>) {
        if self.colors != colors {
            self.colors = colors;
            cx.notify();
        }
    }

    pub fn is_pristine(&self) -> bool {
        self.pristine && self.sending.is_none()
    }

    pub fn has_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Fills the surface with an agent's request. Variables it brought are
    /// merged into the named environment (the active one, or "Local").
    /// Returns whether the request should go out now: only a `GET` marked
    /// `autoSend`.
    pub fn open_draft(&mut self, draft: &diri_proto::ApiRequestDraft, cx: &mut Context<Self>) {
        let request = model::from_draft(draft);
        if let Some(environment) = &draft.environment {
            let variables: Vec<Variable> = environment
                .variables
                .iter()
                .map(|variable| {
                    Variable::new(
                        variable.name.clone(),
                        variable.value.clone(),
                        variable.secret,
                    )
                })
                .collect();
            let name = environment.name.clone().unwrap_or_else(|| {
                self.library
                    .read(cx)
                    .active_environment()
                    .map_or_else(|| "Local".to_owned(), |active| active.name.clone())
            });
            self.library.update(cx, |library, cx| {
                library.merge_environment(&name, variables);
                library.changed(cx);
            });
            self.editing_environment = self.library.read(cx).project.active_environment.clone();
        }
        self.load_request(request, cx);
        if draft.may_auto_send() {
            self.send(cx);
        }
    }

    /// Shows `request` in the editor, clearing the last answer.
    pub fn load_request(&mut self, request: ApiRequest, cx: &mut Context<Self>) {
        self.cancel(cx);
        self.request_tab = if request.method.carries_body() && request.body_kind != BodyKind::None {
            RequestTab::Body
        } else if !request.params.is_empty() {
            RequestTab::Params
        } else if !request.headers.is_empty() {
            RequestTab::Headers
        } else {
            RequestTab::Params
        };
        self.draft = request;
        self.pristine = true;
        self.field = None;
        self.outcome = None;
        self.folded.clear();
        self.mode = Mode::Request;
        cx.notify();
    }

    fn touched(&mut self) {
        self.pristine = false;
    }

    // MARK: Sending

    pub fn is_sending(&self) -> bool {
        self.sending.is_some()
    }

    pub fn send(&mut self, cx: &mut Context<Self>) {
        if self.sending.is_some() {
            return;
        }
        self.commit_field();
        let environment = self.library.read(cx).active_environment().cloned();
        let outgoing = match model::prepare(&self.draft, environment.as_ref()) {
            Ok(outgoing) => outgoing,
            Err(error) => {
                self.outcome = Some(Outcome::Failed(error.message()));
                cx.notify();
                return;
            }
        };
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let request = self.draft.clone();
        self.send_generation = self.send_generation.wrapping_add(1);
        let generation = self.send_generation;
        let job = self.tokio.spawn(http::send(outgoing, cancelled));
        let task = cx.spawn(async move |this, cx| {
            let result = match job.await {
                Ok(result) => result,
                Err(_) => Err(HttpError::Cancelled),
            };
            let _ = this.update(cx, |this, cx| this.finish(generation, request, result, cx));
        });
        self.sending = Some(Sending {
            cancel: Some(cancel),
            _task: task,
        });
        self.method_menu_open = false;
        cx.notify();
    }

    fn finish(
        &mut self,
        generation: u64,
        request: ApiRequest,
        result: Result<ApiResponse, HttpError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.send_generation {
            return;
        }
        self.sending = None;
        let (status, took_ms) = match &result {
            Ok(response) => (Some(response.status), response.took_ms),
            Err(_) => (None, 0),
        };
        if !matches!(result, Err(HttpError::Cancelled)) {
            self.library.update(cx, |library, cx| {
                model::record_history(
                    &mut library.project.history,
                    HistoryEntry {
                        request,
                        status,
                        took_ms,
                        at_ms: model::now_ms(),
                    },
                );
                library.changed(cx);
            });
        }
        self.folded.clear();
        self.body_scroll = UniformListScrollHandle::new();
        self.outcome = Some(match result {
            Ok(response) => Outcome::Response(ResponseView::new(response)),
            Err(error) => Outcome::Failed(error.message()),
        });
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(mut sending) = self.sending.take() {
            if let Some(cancel) = sending.cancel.take() {
                let _ = cancel.send(());
            }
            self.send_generation = self.send_generation.wrapping_add(1);
            self.outcome = Some(Outcome::Failed(HttpError::Cancelled.message()));
            cx.notify();
        }
    }

    // MARK: Fields

    fn field_text(&self, field: Field, cx: &App) -> String {
        let cell = |rows: &[Pair], index: usize, column: Column| {
            rows.get(index)
                .map_or_else(String::new, |pair| match column {
                    Column::Name => pair.name.clone(),
                    Column::Value => pair.value.clone(),
                })
        };
        match field {
            Field::Url => self.draft.url.clone(),
            Field::Name => self.draft.name.clone(),
            Field::Param(index, column) => cell(&self.draft.params, index, column),
            Field::Header(index, column) => cell(&self.draft.headers, index, column),
            Field::Form(index, column) => cell(&self.draft.form, index, column),
            Field::Body => self.draft.body.clone(),
            Field::BearerToken => match &self.draft.auth {
                Auth::Bearer { token } => token.clone(),
                _ => String::new(),
            },
            Field::BasicUser => match &self.draft.auth {
                Auth::Basic { user, .. } => user.clone(),
                _ => String::new(),
            },
            Field::BasicPassword => match &self.draft.auth {
                Auth::Basic { password, .. } => password.clone(),
                _ => String::new(),
            },
            Field::EnvName => self
                .editing_environment(cx)
                .map(|environment| environment.name.clone())
                .unwrap_or_default(),
            Field::Variable(index, column) => self
                .editing_environment(cx)
                .and_then(|environment| environment.variables.get(index))
                .map(|variable| match column {
                    Column::Name => variable.name.clone(),
                    Column::Value => variable.value.clone(),
                })
                .unwrap_or_default(),
            Field::FolderName => {
                fn name_of(items: &[Saved], id: &str) -> Option<String> {
                    items.iter().find_map(|item| match item {
                        Saved::Folder {
                            id: folder, name, ..
                        } if folder == id => Some(name.clone()),
                        Saved::Folder { items, .. } => name_of(items, id),
                        Saved::Request { .. } => None,
                    })
                }
                self.tree_selected
                    .as_deref()
                    .and_then(|id| name_of(&self.library.read(cx).project.collections, id))
                    .unwrap_or_default()
            }
        }
    }

    pub(crate) fn set_field_text(&mut self, field: Field, text: String, cx: &mut Context<Self>) {
        fn set_cell(rows: &mut [Pair], index: usize, column: Column, text: String) {
            if let Some(pair) = rows.get_mut(index) {
                match column {
                    Column::Name => pair.name = text,
                    Column::Value => pair.value = text,
                }
            }
        }
        match field {
            Field::Url => self.draft.set_url(text),
            Field::Name => self.draft.name = text,
            Field::Param(index, column) => {
                let mut params = self.draft.params.clone();
                set_cell(&mut params, index, column, text);
                self.draft.set_params(params);
            }
            Field::Header(index, column) => set_cell(&mut self.draft.headers, index, column, text),
            Field::Form(index, column) => set_cell(&mut self.draft.form, index, column, text),
            Field::Body => self.draft.body = text,
            Field::BearerToken => self.draft.auth = Auth::Bearer { token: text },
            Field::BasicUser | Field::BasicPassword => {
                let (mut user, mut password) = match &self.draft.auth {
                    Auth::Basic { user, password } => (user.clone(), password.clone()),
                    _ => (String::new(), String::new()),
                };
                if field == Field::BasicUser {
                    user = text;
                } else {
                    password = text;
                }
                self.draft.auth = Auth::Basic { user, password };
            }
            Field::EnvName | Field::Variable(..) => {
                let Some(id) = self.editing_environment.clone() else {
                    return;
                };
                self.library.update(cx, |library, cx| {
                    if let Some(environment) = library.environment_mut(&id) {
                        match field {
                            Field::EnvName => environment.name = text,
                            Field::Variable(index, column) => {
                                if let Some(variable) = environment.variables.get_mut(index) {
                                    match column {
                                        Column::Name => variable.name = text,
                                        Column::Value => variable.value = text,
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    library.changed(cx);
                });
                return;
            }
            Field::FolderName => {
                let Some(id) = self.tree_selected.clone() else {
                    return;
                };
                self.library.update(cx, |library, cx| {
                    model::rename_folder(&mut library.project.collections, &id, &text);
                    library.changed(cx);
                });
                return;
            }
        }
        if field != Field::Name {
            self.touched();
        }
        cx.notify();
    }

    pub(crate) fn focus_field(
        &mut self,
        field: Field,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut editor = QueryEditor::default();
        editor.insert_multiline(&self.field_text(field, cx));
        self.field = Some((field, editor));
        self.method_menu_open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn commit_field(&mut self) {
        self.field = None;
    }

    pub(crate) fn focused_field(&self) -> Option<Field> {
        self.field.as_ref().map(|(field, _)| *field)
    }

    /// The fields Tab walks through, in the order they are drawn.
    fn tab_order(&self, cx: &App) -> Vec<Field> {
        let mut fields = Vec::new();
        match self.mode {
            Mode::Request => {
                fields.push(Field::Url);
                let rows =
                    |count: usize, make: fn(usize, Column) -> Field, fields: &mut Vec<Field>| {
                        for index in 0..count {
                            fields.push(make(index, Column::Name));
                            fields.push(make(index, Column::Value));
                        }
                    };
                match self.request_tab {
                    RequestTab::Params => rows(self.draft.params.len(), Field::Param, &mut fields),
                    RequestTab::Headers => {
                        rows(self.draft.headers.len(), Field::Header, &mut fields)
                    }
                    RequestTab::Body => match self.draft.body_kind {
                        BodyKind::Form => rows(self.draft.form.len(), Field::Form, &mut fields),
                        BodyKind::Json | BodyKind::Raw => fields.push(Field::Body),
                        BodyKind::None => {}
                    },
                    RequestTab::Auth => match self.draft.auth {
                        Auth::None => {}
                        Auth::Bearer { .. } => fields.push(Field::BearerToken),
                        Auth::Basic { .. } => {
                            fields.push(Field::BasicUser);
                            fields.push(Field::BasicPassword);
                        }
                    },
                }
            }
            Mode::Environments => {
                if let Some(environment) = self.editing_environment(cx) {
                    fields.push(Field::EnvName);
                    for index in 0..environment.variables.len() {
                        fields.push(Field::Variable(index, Column::Name));
                        fields.push(Field::Variable(index, Column::Value));
                    }
                }
            }
            Mode::Collections | Mode::History => {}
        }
        fields
    }

    fn move_focus(&mut self, backward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.tab_order(cx);
        if order.is_empty() {
            return;
        }
        let current = self
            .focused_field()
            .and_then(|field| order.iter().position(|each| *each == field));
        let next = match (current, backward) {
            (Some(index), false) => (index + 1) % order.len(),
            (Some(index), true) => (index + order.len() - 1) % order.len(),
            (None, _) => 0,
        };
        self.focus_field(order[next], window, cx);
    }

    pub(crate) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let command = keystroke.modifiers.platform;
        match keystroke.key.as_str() {
            "enter" if command => {
                self.send(cx);
                cx.stop_propagation();
                return;
            }
            "s" if command => {
                self.save_to_collection(cx);
                cx.stop_propagation();
                return;
            }
            "." if command && self.sending.is_some() => {
                self.cancel(cx);
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let Some((field, _)) = self.field.as_ref() else {
            if keystroke.key == "escape" && self.method_menu_open {
                self.method_menu_open = false;
                cx.notify();
                cx.stop_propagation();
            }
            return;
        };
        let field = *field;
        match keystroke.key.as_str() {
            "escape" => {
                self.commit_field();
                cx.notify();
            }
            "tab" => self.move_focus(keystroke.modifiers.shift, window, cx),
            "enter" if field == Field::Body => {
                self.edit(field, |editor| editor.insert_multiline("\n"), cx)
            }
            "enter" if field == Field::Url => self.send(cx),
            "enter" => {
                self.commit_field();
                cx.notify();
            }
            _ => match query_editor::edit_for(keystroke) {
                Some(Edit::Local(edit)) => self.edit(field, |editor| editor.apply(edit), cx),
                Some(Edit::Clipboard(ClipboardEdit::Copy)) => {
                    if let Some((_, editor)) = &self.field {
                        query_editor::copy_selection(editor, cx);
                    }
                }
                Some(Edit::Clipboard(ClipboardEdit::Cut)) => {
                    let mut cut = false;
                    if let Some((_, editor)) = &mut self.field {
                        cut = query_editor::cut_selection(editor, cx);
                    }
                    if cut {
                        self.sync_field(field, cx);
                    }
                }
                Some(Edit::Clipboard(ClipboardEdit::Paste)) => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        let multiline = field == Field::Body;
                        self.edit(
                            field,
                            |editor| {
                                if multiline {
                                    editor.insert_multiline(&text)
                                } else {
                                    editor.insert(&text)
                                }
                            },
                            cx,
                        );
                    }
                }
                None => return,
            },
        }
        cx.stop_propagation();
    }

    fn edit(
        &mut self,
        field: Field,
        apply: impl FnOnce(&mut QueryEditor) -> bool,
        cx: &mut Context<Self>,
    ) {
        let changed = match &mut self.field {
            Some((_, editor)) => apply(editor),
            None => false,
        };
        if changed {
            self.sync_field(field, cx);
        } else {
            cx.notify();
        }
    }

    /// Copies the focused editor's text into the model it edits.
    fn sync_field(&mut self, field: Field, cx: &mut Context<Self>) {
        let Some(text) = self
            .field
            .as_ref()
            .map(|(_, editor)| editor.text().to_owned())
        else {
            return;
        };
        self.set_field_text(field, text, cx);
    }

    // MARK: Request edits

    pub fn set_method(&mut self, method: Method, cx: &mut Context<Self>) {
        self.method_menu_open = false;
        if self.draft.method != method {
            self.draft.method = method;
            self.touched();
        }
        cx.notify();
    }

    pub fn set_body_kind(&mut self, kind: BodyKind, cx: &mut Context<Self>) {
        self.commit_field();
        self.draft.body_kind = kind;
        if kind == BodyKind::Form && self.draft.form.is_empty() {
            self.draft.form.push(Pair::new("", ""));
        }
        self.touched();
        cx.notify();
    }

    pub fn set_auth_kind(&mut self, auth: Auth, cx: &mut Context<Self>) {
        self.commit_field();
        if std::mem::discriminant(&self.draft.auth) != std::mem::discriminant(&auth) {
            self.draft.auth = auth;
            self.touched();
        }
        cx.notify();
    }

    pub(crate) fn add_row(&mut self, tab: RequestTab, window: &mut Window, cx: &mut Context<Self>) {
        let field = match tab {
            RequestTab::Params => {
                let mut params = self.draft.params.clone();
                params.push(Pair::new("", ""));
                let index = params.len() - 1;
                self.draft.set_params(params);
                Field::Param(index, Column::Name)
            }
            RequestTab::Headers => {
                self.draft.headers.push(Pair::new("", ""));
                Field::Header(self.draft.headers.len() - 1, Column::Name)
            }
            RequestTab::Body => {
                self.draft.form.push(Pair::new("", ""));
                Field::Form(self.draft.form.len() - 1, Column::Name)
            }
            RequestTab::Auth => return,
        };
        self.touched();
        self.focus_field(field, window, cx);
    }

    pub(crate) fn remove_row(&mut self, tab: RequestTab, index: usize, cx: &mut Context<Self>) {
        self.commit_field();
        match tab {
            RequestTab::Params => {
                let mut params = self.draft.params.clone();
                if index < params.len() {
                    params.remove(index);
                }
                self.draft.set_params(params);
            }
            RequestTab::Headers => {
                if index < self.draft.headers.len() {
                    self.draft.headers.remove(index);
                }
            }
            RequestTab::Body => {
                if index < self.draft.form.len() {
                    self.draft.form.remove(index);
                }
            }
            RequestTab::Auth => {}
        }
        self.touched();
        cx.notify();
    }

    pub(crate) fn toggle_row(&mut self, tab: RequestTab, index: usize, cx: &mut Context<Self>) {
        self.commit_field();
        match tab {
            RequestTab::Params => {
                let mut params = self.draft.params.clone();
                if let Some(pair) = params.get_mut(index) {
                    pair.enabled = !pair.enabled;
                }
                self.draft.set_params(params);
            }
            RequestTab::Headers => {
                if let Some(pair) = self.draft.headers.get_mut(index) {
                    pair.enabled = !pair.enabled;
                }
            }
            RequestTab::Body => {
                if let Some(pair) = self.draft.form.get_mut(index) {
                    pair.enabled = !pair.enabled;
                }
            }
            RequestTab::Auth => {}
        }
        self.touched();
        cx.notify();
    }

    pub fn format_body(&mut self, cx: &mut Context<Self>) {
        if let Some(formatted) = json::format_body(&self.draft.body) {
            self.commit_field();
            self.draft.body = formatted;
            self.touched();
            cx.notify();
        }
    }

    // MARK: Collections, history, environments

    /// Saves the draft into the collection: over its saved copy when it has
    /// one, otherwise into the selected folder (or the root).
    pub fn save_to_collection(&mut self, cx: &mut Context<Self>) {
        self.commit_field();
        let mut request = self.draft.clone();
        if request.name.trim().is_empty() {
            request.name = request.title();
            self.draft.name = request.name.clone();
        }
        let folder = self.selected_folder(cx);
        self.library.update(cx, |library, cx| {
            let collections = &mut library.project.collections;
            if !model::replace_request(collections, &request) {
                model::insert_into(collections, folder.as_deref(), Saved::Request { request });
            }
            library.changed(cx);
        });
        if let Some(folder) = folder {
            self.tree_open.insert(folder);
        }
        self.pristine = true;
        cx.notify();
    }

    fn selected_folder(&self, cx: &App) -> Option<String> {
        let selected = self.tree_selected.as_deref()?;
        fn is_folder(items: &[Saved], id: &str) -> bool {
            items.iter().any(|item| match item {
                Saved::Folder {
                    id: folder, items, ..
                } => folder == id || is_folder(items, id),
                Saved::Request { .. } => false,
            })
        }
        is_folder(&self.library.read(cx).project.collections, selected).then(|| selected.to_owned())
    }

    pub fn is_saved(&self, cx: &App) -> bool {
        model::find_request(&self.library.read(cx).project.collections, &self.draft.id).is_some()
    }

    pub fn new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = model::fresh_id("folder");
        let parent = self.selected_folder(cx);
        self.library.update(cx, |library, cx| {
            model::insert_into(
                &mut library.project.collections,
                parent.as_deref(),
                Saved::Folder {
                    id: id.clone(),
                    name: "New folder".to_owned(),
                    items: Vec::new(),
                },
            );
            library.changed(cx);
        });
        if let Some(parent) = parent {
            self.tree_open.insert(parent);
        }
        self.tree_selected = Some(id);
        self.focus_field(Field::FolderName, window, cx);
    }

    pub(crate) fn activate_tree_row(&mut self, id: &str, cx: &mut Context<Self>) {
        self.commit_field();
        self.tree_selected = Some(id.to_owned());
        let request = model::find_request(&self.library.read(cx).project.collections, id).cloned();
        match request {
            Some(request) => self.load_request(request, cx),
            None => {
                if !self.tree_open.remove(id) {
                    self.tree_open.insert(id.to_owned());
                }
                cx.notify();
            }
        }
    }

    pub(crate) fn delete_tree_item(&mut self, id: &str, cx: &mut Context<Self>) {
        self.commit_field();
        self.library.update(cx, |library, cx| {
            model::remove_item(&mut library.project.collections, id);
            library.changed(cx);
        });
        if self.tree_selected.as_deref() == Some(id) {
            self.tree_selected = None;
        }
        cx.notify();
    }

    pub(crate) fn open_history(&mut self, index: usize, cx: &mut Context<Self>) {
        let entry = self.library.read(cx).project.history.get(index).cloned();
        if let Some(entry) = entry {
            let mut request = entry.request;
            // A request that is no longer saved comes back as a new one, so
            // Save does not resurrect a deleted id.
            if model::find_request(&self.library.read(cx).project.collections, &request.id)
                .is_none()
            {
                request.id = model::fresh_id("req");
            }
            self.load_request(request, cx);
        }
    }

    pub fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.library.update(cx, |library, cx| {
            library.project.history.clear();
            library.changed(cx);
        });
    }

    fn editing_environment<'a>(&self, cx: &'a App) -> Option<&'a Environment> {
        let id = self.editing_environment.as_ref()?;
        self.library
            .read(cx)
            .project
            .environments
            .iter()
            .find(|environment| &environment.id == id)
    }

    pub fn set_active_environment(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.commit_field();
        self.editing_environment = id.clone();
        self.library.update(cx, |library, cx| {
            library.project.active_environment = id;
            library.changed(cx);
        });
    }

    pub fn new_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = model::fresh_id("env");
        let count = self.library.read(cx).project.environments.len();
        self.library.update(cx, |library, cx| {
            library.project.environments.push(Environment {
                id: id.clone(),
                name: if count == 0 {
                    "Local".to_owned()
                } else {
                    format!("Environment {}", count + 1)
                },
                variables: vec![Variable::new("baseUrl", "http://localhost:3000", false)],
            });
            library.project.active_environment = Some(id.clone());
            library.changed(cx);
        });
        self.editing_environment = Some(id);
        self.focus_field(Field::EnvName, window, cx);
    }

    pub fn delete_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.commit_field();
        self.library.update(cx, |library, cx| {
            library
                .project
                .environments
                .retain(|environment| environment.id != id);
            if library.project.active_environment.as_deref() == Some(id) {
                library.project.active_environment = None;
            }
            library.changed(cx);
        });
        if self.editing_environment.as_deref() == Some(id) {
            self.editing_environment = self.library.read(cx).project.active_environment.clone();
        }
        self.revealed.retain(|(environment, _)| environment != id);
        cx.notify();
    }

    pub(crate) fn add_variable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing_environment.clone() else {
            return;
        };
        let mut index = None;
        self.library.update(cx, |library, cx| {
            if let Some(environment) = library.environment_mut(&id) {
                environment.variables.push(Variable::new("", "", false));
                index = Some(environment.variables.len() - 1);
            }
            library.changed(cx);
        });
        if let Some(index) = index {
            self.focus_field(Field::Variable(index, Column::Name), window, cx);
        }
    }

    pub(crate) fn update_variable(
        &mut self,
        index: usize,
        change: impl FnOnce(&mut Vec<Variable>),
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.editing_environment.clone() else {
            return;
        };
        self.commit_field();
        self.library.update(cx, |library, cx| {
            if let Some(environment) = library.environment_mut(&id) {
                change(&mut environment.variables);
            }
            library.changed(cx);
        });
        self.revealed
            .retain(|(environment, revealed)| environment != &id || *revealed != index);
        cx.notify();
    }

    pub(crate) fn toggle_reveal(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(id) = self.editing_environment.clone() else {
            return;
        };
        let key = (id, index);
        if !self.revealed.remove(&key) {
            self.revealed.insert(key);
        }
        cx.notify();
    }

    // MARK: Response

    pub(crate) fn toggle_fold(&mut self, line: usize, cx: &mut Context<Self>) {
        if !self.folded.remove(&line) {
            self.folded.insert(line);
        }
        cx.notify();
    }

    pub(crate) fn fold_all(&mut self, folded: bool, cx: &mut Context<Self>) {
        self.folded = match (&self.outcome, folded) {
            (Some(Outcome::Response(view)), true) => view
                .pretty
                .as_ref()
                .map(|pretty| pretty.folds_at_depth(1))
                .unwrap_or_default(),
            _ => HashSet::new(),
        };
        cx.notify();
    }

    pub(crate) fn copy_response(&self, cx: &mut Context<Self>) {
        if let Some(Outcome::Response(view)) = &self.outcome {
            let text = match (&view.pretty, self.response_raw) {
                (Some(pretty), false) => pretty.text(),
                _ => view.response.body_text(),
            };
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }
}
