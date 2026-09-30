//! Diri Notes for agents: discover, read, and add to the user's notes.
//!
//! Notes live outside project repos, in Diri's app-support directory, so
//! agents find them here rather than on disk: the notes for their project,
//! the notes that mention them, and the note they were started from. A note
//! in the sidebar is a Session of kind `note` whose `note_id` names its file;
//! "the note I was started from" is the nearest such Session among the
//! caller's ancestors. Reads need no Engine; anything about sessions (note
//! sessions, who is mentioned, their status, write authorization) comes from
//! `session.list`.

use super::*;
use diri_notes::handoff::{self, TodoSelector};
use diri_notes::mention::{self, MentionTarget};
use diri_notes::store::{self as note_store, Note, NoteMeta, NoteStore, Resolve};
use diri_proto::ProjectId;

const DEFAULT_NOTE_LIMIT: u64 = 50;
const MAX_NOTE_LIMIT: u64 = 200;
const MAX_APPEND_BYTES: usize = 64 * 1024;
const ORIGIN: &str = "origin";
const NOTE_SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

/// What happened when the CLI asked the Engine to create a note Session.
#[derive(Debug)]
pub enum NoteSpawn {
    Created(Box<SessionRecord>),
    /// No Engine to ask, or one that predates note Sessions: the caller
    /// writes the file directly and the note has no Session yet.
    Unavailable(String),
}

impl Bridge {
    /// Creates a note Session (and its file) through `session.spawn`, so the
    /// note appears in the sidebar under `parent`. `cwd` is the project root.
    /// Only an unreachable or too-old Engine yields `Unavailable`; an Engine
    /// that rejects the request is an error, never a silent fallback.
    pub fn spawn_note(
        &self,
        cwd: &str,
        title: &str,
        body: &str,
        parent: Option<&str>,
    ) -> Result<NoteSpawn, String> {
        let params = SessionSpawnParams {
            account_profile_id: None,
            kind: AgentKind::NOTE,
            cwd: cwd.to_owned(),
            new_worktree: None,
            worktree_branch: None,
            worktree_base: None,
            title: Some(title.to_owned()).filter(|t| !t.trim().is_empty()),
            initial_prompt: Some(body.to_owned()).filter(|b| !b.trim().is_empty()),
            parent: parent.map(SessionId::new),
            initial_cols: None,
            initial_rows: None,
            host: None,
            same_repo_as: None,
            start_directory: None,
        };
        let params = serde_json::to_value(params).map_err(|e| e.to_string())?;
        let mut client = match self.connect(NOTE_SPAWN_TIMEOUT) {
            Ok(client) => client,
            Err(failure) => return Ok(NoteSpawn::Unavailable(render_failure(failure))),
        };
        let deadline = Instant::now() + NOTE_SPAWN_TIMEOUT;
        match client.request_until(Method::SESSION_SPAWN.into(), params, deadline) {
            Ok(value) => {
                let record: SessionRecord = serde_json::from_value(value)
                    .map_err(|e| format!("invalid session.spawn response: {e}"))?;
                if !record.is_note() || record.note_id.is_none() {
                    return Err(format!(
                        "the Engine created session {} but not a note",
                        record.id.0
                    ));
                }
                Ok(NoteSpawn::Created(Box::new(record)))
            }
            Err(ControlFailure::Daemon(error))
                if error.message.contains("no manifest for agent") =>
            {
                Ok(NoteSpawn::Unavailable(
                    "the running Diri Engine predates note sessions".into(),
                ))
            }
            Err(failure) => Err(render_failure(failure)),
        }
    }

    pub(super) fn list_notes(&self, args: &Value) -> Result<Value, String> {
        let store = self.note_store()?;
        let notes = store
            .list()
            .map_err(|e| format!("cannot list notes: {e}"))?;
        let include_archived = optional_bool(args, "include_archived").unwrap_or(false);
        let query = optional_string(args, "query").map(|q| q.to_lowercase());
        let limit = args["limit"]
            .as_u64()
            .unwrap_or(DEFAULT_NOTE_LIMIT)
            .clamp(1, MAX_NOTE_LIMIT) as usize;
        let project = optional_string(args, "project")
            .unwrap_or_else(|| if self.caller.is_some() { "mine" } else { "all" }.to_owned());
        let mentions = optional_string(args, "mentions");

        // Session facts are optional unless the filter itself needs them.
        let snapshot = self.snapshot();
        let needed = || {
            snapshot
                .as_ref()
                .map_err(|error| format!("this filter needs the Diri Engine: {error}"))
        };
        let (project_root, project_id) = match project.as_str() {
            "all" => (None, None),
            "mine" => {
                let (root, id) = self.caller_project(needed()?)?;
                (Some(root), Some(id))
            }
            path => (Some(path.to_owned()), None),
        };
        let sessions: &[SessionRecord] = snapshot.as_ref().map_or(&[], |s| &s.sessions);
        let by_note = note_sessions(sessions);
        // Session ids that count as "mentioned", each with how it relates to
        // the subject: the subject itself, or one of its ancestors.
        let mentioned: Option<HashMap<String, &'static str>> = match mentions.as_deref() {
            None => None,
            Some(subject) => {
                let subject = if subject == "me" {
                    self.require_caller()?.to_owned()
                } else {
                    subject.to_owned()
                };
                let lineage = Lineage::new(&needed()?.sessions, Some(&subject));
                let mut ids = HashMap::from([(subject.clone(), "self")]);
                for ancestor in lineage.ancestors(&subject) {
                    ids.entry(ancestor.id.0.clone()).or_insert("ancestor");
                }
                Some(ids)
            }
        };

        let mut rows = Vec::new();
        let mut total = 0;
        for note in &notes {
            let session = by_note.get(note.id.as_str()).copied();
            if (note.archived || session.is_some_and(SessionRecord::is_archived))
                && !include_archived
            {
                continue;
            }
            if let Some(root) = &project_root {
                let by_file = note
                    .project
                    .as_deref()
                    .is_some_and(|p| policy::same_path(p, root));
                let by_session = session
                    .zip(project_id.as_ref())
                    .is_some_and(|(record, id)| &record.project_id == id);
                if !by_file && !by_session {
                    continue;
                }
            }
            if let Some(query) = &query
                && !note.haystack.contains(query)
            {
                continue;
            }
            let via = match &mentioned {
                None => None,
                Some(ids) => {
                    let via = note.mentions.iter().find_map(|target| match target {
                        MentionTarget::Session(id) => ids.get(id).copied(),
                        MentionTarget::Note(_) => None,
                    });
                    if via.is_none() {
                        continue;
                    }
                    via
                }
            };
            total += 1;
            if rows.len() < limit {
                let mut row = note_row(note);
                if let Some(record) = session {
                    row["session_id"] = json!(record.id.0);
                }
                if let Some(via) = via {
                    row["mentioned_via"] = json!(via);
                }
                rows.push(row);
            }
        }
        Ok(json!({
            "notes": rows,
            "total": total,
            "truncated": total > rows.len(),
            "project": project_root,
            "notes_dir": store.dir(),
        }))
    }

    pub(super) fn read_note(&self, args: &Value) -> Result<Value, String> {
        let store = self.note_store()?;
        // Notes are readable without Diri running; session facts are extra.
        let sessions = self.sessions();
        let meta = self.resolve_note(
            &store,
            &required_string(args, "note")?,
            sessions.as_deref().ok(),
        )?;
        let note = store
            .load(&meta.id)
            .map_err(|e| format!("cannot read note {}: {e}", meta.id))?;
        let find = |id: &str| {
            sessions
                .as_ref()
                .ok()
                .and_then(|all| all.iter().find(|record| record.id.0 == id))
        };
        let session_value = |id: &str| match (find(id), sessions.is_ok()) {
            (Some(record), _) => session_fact(record),
            (None, true) => json!({"id": id, "missing": true}),
            // Without the Engine a session is unknown, not missing.
            (None, false) => json!({"id": id}),
        };

        let todos: Vec<Value> = handoff::todos(&note)
            .into_iter()
            .map(|todo| {
                json!({
                    "index": todo.index,
                    "text": todo.text,
                    "checked": todo.checked,
                    "sessions": todo.sessions.iter().map(|id| session_value(id)).collect::<Vec<_>>(),
                })
            })
            .collect();
        let notes = store.list().unwrap_or_default();
        let mentions: Vec<Value> = note
            .doc
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(index, block)| {
                mention::in_block(block)
                    .into_iter()
                    .map(move |chip| (index, block.text[chip.range.clone()].to_owned(), chip))
            })
            .map(|(block, label, mention)| {
                let mut value = json!({"label": label, "block": block});
                match &mention.target {
                    MentionTarget::Session(id) => {
                        value["session"] = session_value(id);
                    }
                    MentionTarget::Note(id) => {
                        value["note"] = match notes.iter().find(|n| &n.id == id) {
                            Some(other) => json!({"id": id, "title": other.display_title()}),
                            None => json!({"id": id, "missing": true}),
                        };
                    }
                }
                value
            })
            .collect();

        let mut result = note_row(&meta);
        let object = result.as_object_mut().expect("note_row is an object");
        if let Some(record) = sessions
            .as_deref()
            .ok()
            .and_then(|all| note_sessions(all).get(meta.id.as_str()).copied())
        {
            object.insert("session".into(), session_fact(record));
        }
        object.insert("markdown".into(), json!(note.to_markdown()));
        object.insert("todos".into(), Value::Array(todos));
        object.insert("mentions".into(), Value::Array(mentions));
        if let Err(error) = sessions {
            object.insert("sessions_unavailable".into(), json!(error));
        }
        Ok(result)
    }

    pub(super) fn write_note(&self, args: &Value) -> Result<Value, String> {
        let append = optional_string(args, "append");
        let checked = optional_bool(args, "checked");
        let link = optional_string(args, "link_session");
        let todo = match (args["todo_index"].as_u64(), optional_string(args, "todo")) {
            (Some(index), None) => Some(TodoSelector::Index(index as usize)),
            (None, Some(text)) => Some(TodoSelector::Text(text)),
            (None, None) => None,
            (Some(_), Some(_)) => return Err("pass todo or todo_index, not both".into()),
        };
        if append.is_none() && checked.is_none() && link.is_none() {
            return Err("write_note needs append, checked, or link_session".into());
        }
        if todo.is_none() && (checked.is_some() || link.is_some()) {
            return Err("checked and link_session need todo or todo_index".into());
        }
        if todo.is_some() && checked.is_none() && link.is_none() {
            return Err("todo selects a to-do for checked or link_session; pass one".into());
        }
        if append
            .as_ref()
            .is_some_and(|text| text.len() > MAX_APPEND_BYTES)
        {
            return Err(format!("append is larger than {MAX_APPEND_BYTES} bytes"));
        }

        let store = self.note_store()?;
        let snapshot = self.snapshot()?;
        let meta = self.resolve_note(
            &store,
            &required_string(args, "note")?,
            Some(&snapshot.sessions),
        )?;
        let note_session = note_sessions(&snapshot.sessions)
            .get(meta.id.as_str())
            .map(|record| record.id.0.clone());
        let mentioned: Vec<String> = meta
            .mentions
            .iter()
            .filter_map(|target| match target {
                MentionTarget::Session(id) => Some(id.clone()),
                MentionTarget::Note(_) => None,
            })
            .collect();
        McpPolicy::new(
            &snapshot.sessions,
            &snapshot.projects,
            self.caller.as_deref(),
        )?
        .authorize(WriteAction::WriteNote {
            mentions: &mentioned,
            note_session: note_session.as_deref(),
        })?;
        let link = match link {
            Some(id) => {
                let record = find_session(&snapshot.sessions, &id)?;
                let label = mention::session_label(
                    short_label(record.effective_kind().id()),
                    &record.title,
                );
                Some((id, label))
            }
            None => None,
        };

        let (note, (index, changes)) = store
            .update(&meta.id, |note: &mut Note| {
                let index = match &todo {
                    Some(selector) => {
                        Some(handoff::find_todo(note, selector).map_err(|e| {
                            std::io::Error::new(std::io::ErrorKind::InvalidInput, e)
                        })?)
                    }
                    None => None,
                };
                let mut changes = Vec::new();
                if let (Some(index), Some(checked)) = (index, checked)
                    && handoff::set_checked(note, index, checked)
                {
                    changes.push(if checked { "checked" } else { "unchecked" });
                }
                if let (Some(index), Some((id, label))) = (index, &link)
                    && handoff::link_session(note, index, label, id)
                {
                    changes.push("linked");
                }
                if let Some(text) = &append {
                    note_store::append_markdown(note, text);
                    changes.push("appended");
                }
                Ok((index, changes))
            })
            .map_err(|e| format!("cannot write note {}: {e}", meta.id))?;
        let mut result = json!({"note": meta.id, "changes": changes});
        if let Some(index) = index
            && let Some(todo) = handoff::todos(&note).into_iter().find(|t| t.index == index)
        {
            result["todo"] = json!({
                "index": todo.index,
                "text": todo.text,
                "checked": todo.checked,
                "sessions": todo.sessions,
            });
        }
        Ok(result)
    }

    fn note_store(&self) -> Result<NoteStore, String> {
        let dir = self
            .notes_dir
            .clone()
            .or_else(NoteStore::resolve_dir)
            .ok_or("cannot find a home directory for notes")?;
        NoteStore::open(dir).map_err(|e| format!("cannot open notes: {e}"))
    }

    /// A note by id or title, by its note Session's id, or `origin`: the
    /// nearest note Session among the caller's ancestors.
    fn resolve_note(
        &self,
        store: &NoteStore,
        query: &str,
        sessions: Option<&[SessionRecord]>,
    ) -> Result<NoteMeta, String> {
        let by_session = |id: &str| {
            sessions
                .and_then(|all| all.iter().find(|record| record.id.0 == id))
                .filter(|record| record.is_note())
                .and_then(|record| record.note_id.clone())
        };
        let query = if query == ORIGIN {
            let caller = self.require_caller()?;
            let sessions =
                sessions.ok_or("\"origin\" needs the Diri Engine, which is not reachable")?;
            Lineage::new(sessions, Some(caller))
                .ancestors(caller)
                .into_iter()
                .find(|record| record.is_note())
                .and_then(|record| record.note_id.clone())
                .ok_or("this session was not started from a note (no note session among its ancestors)")?
        } else {
            by_session(query).unwrap_or_else(|| query.to_owned())
        };
        let query = query.as_str();
        let notes = store
            .list()
            .map_err(|e| format!("cannot list notes: {e}"))?;
        match note_store::resolve(&notes, query) {
            Resolve::Found(note) => Ok(*note),
            Resolve::NotFound => Err(format!("no note matches \"{query}\"")),
            Resolve::Ambiguous(many) => Err(format!(
                "\"{query}\" matches {} notes; pass an id: {}",
                many.len(),
                many.iter()
                    .map(|n| format!("{} ({})", n.display_title(), n.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// The caller's project root and id.
    fn caller_project(&self, snapshot: &SessionListResult) -> Result<(String, ProjectId), String> {
        let caller = self.require_caller()?;
        let record = find_session(&snapshot.sessions, caller)?;
        snapshot
            .projects
            .iter()
            .find(|project| project.id == record.project_id)
            .map(|project| (project.root.clone(), project.id.clone()))
            .ok_or_else(|| {
                format!(
                    "calling session {caller} has no live project; pass project:\"all\" or a path"
                )
            })
    }

    /// Appends a child's report to its parent note's Updates section.
    pub(super) fn report_into_note(
        &self,
        note: &SessionRecord,
        caller: &SessionRecord,
        status: &str,
        summary: &str,
        artifacts: &[String],
    ) -> Result<Value, String> {
        let note_id = note
            .note_id
            .as_deref()
            .ok_or_else(|| format!("note session {} has no note file", note.id.0))?;
        let store = self.note_store()?;
        let label =
            mention::session_label(short_label(caller.effective_kind().id()), &caller.title);
        let mut text = if status == "update" {
            summary.to_owned()
        } else {
            format!("{status}: {summary}")
        };
        if !artifacts.is_empty() {
            text.push_str(&format!(" ({})", artifacts.join(", ")));
        }
        let date = diri_notes::store::format_timestamp(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        );
        let mut place = handoff::UpdatePlace::Updates;
        store
            .update(note_id, |doc| {
                place = handoff::append_update(doc, &date, &label, &caller.id.0, &text);
                Ok(())
            })
            .map_err(|e| format!("cannot write note {note_id}: {e}"))?;
        let (todo, where_) = match place {
            handoff::UpdatePlace::Todo(index) => (
                json!(index),
                "Your parent is a note: the report was added under the to-do you were started from.",
            ),
            handoff::UpdatePlace::Updates => (
                Value::Null,
                "Your parent is a note: the report was added to its Updates section.",
            ),
        };
        Ok(json!({
            "ok": true,
            "parent": note.id.0,
            "status": status,
            "recorded_in_note": note_id,
            "todo": todo,
            "note": where_,
        }))
    }
}

/// Note Sessions by the file they show. A file has at most one live
/// Session; when an archived one lingers beside it the live one wins.
fn note_sessions(sessions: &[SessionRecord]) -> HashMap<&str, &SessionRecord> {
    let mut map: HashMap<&str, &SessionRecord> = HashMap::new();
    for record in sessions.iter().filter(|record| record.is_note()) {
        let Some(note_id) = record.note_id.as_deref() else {
            continue;
        };
        match map.get(note_id) {
            Some(existing) if !existing.is_archived() => {}
            _ => {
                map.insert(note_id, record);
            }
        }
    }
    map
}

fn note_row(note: &NoteMeta) -> Value {
    json!({
        "id": note.id,
        "title": note.display_title(),
        "snippet": note.snippet,
        "project": note.project,
        "pinned": note.pinned,
        "archived": note.archived,
        "modified_ms": note.modified_ms,
        "todos": {"done": note.todos_done, "total": note.todos_total, "open": note.open_todos.len()},
        "mentions": note.mentions.iter().map(|target| match target {
            MentionTarget::Session(id) => json!({"session": id}),
            MentionTarget::Note(id) => json!({"note": id}),
        }).collect::<Vec<_>>(),
    })
}

fn session_fact(record: &SessionRecord) -> Value {
    let mut value = compact(record);
    let live = !record.is_archived() && !matches!(record.status, SessionStatus::Exited(_));
    value["live"] = json!(live);
    value
}

#[cfg(test)]
mod tests {
    use super::super::audit_tests::Peer;
    use super::*;
    use diri_notes::doc::Document;

    struct Fixture {
        peer: Peer,
        notes: tempfile::TempDir,
    }

    impl Fixture {
        fn new(sessions: Vec<SessionRecord>) -> Self {
            let listing = json!({
                "sessions": sessions,
                "projects": [{"id": "p", "root": "/work/diri", "name": "diri"}],
            });
            Self {
                peer: Peer::new(move |_| listing.clone()),
                notes: tempfile::tempdir().unwrap(),
            }
        }

        fn store(&self) -> NoteStore {
            NoteStore::open(self.notes.path()).unwrap()
        }

        fn bridge(&self, caller: &str) -> Bridge {
            Bridge::new(self.peer.path.clone(), Some(caller.into()))
                .with_notes_dir(self.notes.path().to_owned())
        }

        fn note(&self, title: &str, project: Option<&str>, body: &str) -> String {
            let store = self.store();
            let (id, _) = store
                .create(Document::new(title, Vec::new()), project)
                .unwrap();
            store.append(&id, body).unwrap();
            id
        }
    }

    fn sessions() -> Vec<SessionRecord> {
        let root = super::super::tests::record("root", None);
        let child = super::super::tests::record("child", Some("root"));
        let other = super::super::tests::record("other", None);
        vec![root, child, other]
    }

    fn ids(value: &Value) -> Vec<String> {
        value["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["title"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn lists_notes_for_the_callers_project_by_default() {
        let fixture = Fixture::new(sessions());
        fixture.note("Diri PRD", Some("/work/diri"), "- [ ] ship");
        fixture.note("Groceries", None, "milk");
        fixture.note("Other repo", Some("/work/else"), "x");

        let mine = fixture
            .bridge("root")
            .call("list_notes", &json!({}))
            .unwrap();
        assert_eq!(ids(&mine), vec!["Diri PRD"]);
        assert_eq!(mine["notes"][0]["todos"]["open"], 1);

        let all = fixture
            .bridge("root")
            .call("list_notes", &json!({"project": "all", "query": "milk"}))
            .unwrap();
        assert_eq!(ids(&all), vec!["Groceries"]);
    }

    #[test]
    fn finds_notes_that_mention_the_caller_or_its_ancestors() {
        let fixture = Fixture::new(sessions());
        fixture.note("Direct", None, "ask [@Child](diri://session/child)");
        fixture.note("Via parent", None, "ask [@Root](diri://session/root)");
        fixture.note("Unrelated", None, "ask [@Other](diri://session/other)");

        let found = fixture
            .bridge("child")
            .call("list_notes", &json!({"project": "all", "mentions": "me"}))
            .unwrap();
        let mut titles = ids(&found);
        titles.sort();
        assert_eq!(titles, vec!["Direct", "Via parent"]);
        for note in found["notes"].as_array().unwrap() {
            let expected = if note["title"] == "Direct" {
                "self"
            } else {
                "ancestor"
            };
            assert_eq!(note["mentioned_via"], expected);
        }
    }

    #[test]
    fn read_note_resolves_mentions_and_todo_sessions() {
        let fixture = Fixture::new(sessions());
        let other = fixture.note("Design", None, "text");
        fixture.note(
            "Resize PRD",
            Some("/work/diri"),
            &format!(
                "See [@Design](diri://note/{other}) and [@Gone](diri://session/s_gone).\n\n\
                 - [ ] Fix flicker [@Codex](diri://session/other)"
            ),
        );
        let note = fixture
            .bridge("root")
            .call("read_note", &json!({"note": "resize"}))
            .unwrap();
        assert_eq!(note["title"], "Resize PRD");
        assert!(
            note["markdown"]
                .as_str()
                .unwrap()
                .contains("- [ ] Fix flicker")
        );
        let mentions = note["mentions"].as_array().unwrap();
        assert_eq!(mentions[0]["note"]["title"], "Design");
        assert_eq!(mentions[1]["session"]["missing"], true);
        assert_eq!(mentions[2]["session"]["id"], "other");
        assert_eq!(mentions[2]["session"]["live"], true);
        let todo = &note["todos"][0];
        assert_eq!(todo["text"], "Fix flicker @Codex");
        assert_eq!(todo["sessions"][0]["id"], "other");

        let origin = fixture
            .bridge("child")
            .call("read_note", &json!({"note": "origin"}))
            .unwrap_err();
        assert!(origin.contains("not started from a note"), "{origin}");
    }

    #[test]
    fn write_note_checks_links_and_appends() {
        let fixture = Fixture::new(sessions());
        let id = fixture.note("Plan", None, "- [ ] Fix flicker\n- [ ] Fix tests");
        let result = fixture
            .bridge("root")
            .call(
                "write_note",
                &json!({"note": id, "todo": "flicker", "checked": true, "link_session": "child", "append": "Done in PR #600."}),
            )
            .unwrap();
        assert_eq!(result["changes"], json!(["checked", "linked", "appended"]));
        assert_eq!(result["todo"]["sessions"], json!(["child"]));
        let source = fixture.store().load(&id).unwrap().to_markdown();
        assert!(
            source.contains("- [x] Fix flicker [@codex: child](diri://session/child)"),
            "{source}"
        );
        assert!(source.trim_end().ends_with("Done in PR #600."), "{source}");
    }

    #[test]
    fn delegated_agents_write_only_to_notes_that_mention_them() {
        let fixture = Fixture::new(sessions());
        let unrelated = fixture.note("Private", None, "mine");
        let about = fixture.note("About root", None, "ask [@Root](diri://session/root)");

        let denied = fixture
            .bridge("child")
            .call("write_note", &json!({"note": unrelated, "append": "hi"}))
            .unwrap_err();
        assert!(denied.contains("write_note denied"), "{denied}");
        fixture
            .bridge("child")
            .call("write_note", &json!({"note": about, "append": "hi"}))
            .unwrap();
        fixture
            .bridge("other")
            .call(
                "write_note",
                &json!({"note": unrelated, "append": "root may"}),
            )
            .unwrap();
    }

    #[test]
    fn write_note_rejects_incomplete_requests_before_touching_files() {
        let fixture = Fixture::new(sessions());
        let id = fixture.note("Plan", None, "- [ ] a");
        let bridge = fixture.bridge("root");
        for args in [
            json!({"note": id}),
            json!({"note": id, "checked": true}),
            json!({"note": id, "todo": "a"}),
            json!({"note": id, "todo": "a", "todo_index": 0, "checked": true}),
        ] {
            assert!(bridge.call("write_note", &args).is_err(), "{args}");
        }
    }

    fn note_session(id: &str, note_id: &str, parent: Option<&str>) -> SessionRecord {
        let mut record = super::super::tests::record(id, parent);
        record.kind = AgentKind::NOTE;
        record.note_id = Some(note_id.into());
        record.title = "Resize PRD".into();
        record
    }

    /// A note session `s_note` with a child agent `child` started from it.
    fn from_note() -> (Fixture, String) {
        let notes = tempfile::tempdir().unwrap();
        let (note_id, _) = NoteStore::open(notes.path())
            .unwrap()
            .create(Document::new("Resize PRD", Vec::new()), Some("/elsewhere"))
            .unwrap();
        let mut fixture = Fixture::new(vec![
            note_session("s_note", &note_id, None),
            super::super::tests::record("child", Some("s_note")),
            super::super::tests::record("grandchild", Some("child")),
            super::super::tests::record("stranger", None),
        ]);
        fixture.notes = notes;
        (fixture, note_id)
    }

    #[test]
    fn origin_is_the_nearest_note_session_among_ancestors() {
        let (fixture, note_id) = from_note();
        for caller in ["child", "grandchild"] {
            let note = fixture
                .bridge(caller)
                .call("read_note", &json!({"note": "origin"}))
                .unwrap();
            assert_eq!(note["id"], note_id.as_str());
            assert_eq!(note["session"]["id"], "s_note");
        }
        let by_session = fixture
            .bridge("stranger")
            .call("read_note", &json!({"note": "s_note"}))
            .unwrap();
        assert_eq!(by_session["id"], note_id.as_str());
        let none = fixture
            .bridge("stranger")
            .call("read_note", &json!({"note": "origin"}))
            .unwrap_err();
        assert!(none.contains("not started from a note"), "{none}");

        let whoami = fixture
            .bridge("grandchild")
            .call("whoami", &json!({}))
            .unwrap();
        assert_eq!(whoami["origin_note"]["note_id"], note_id.as_str());
        assert_eq!(whoami["origin_note"]["session_id"], "s_note");
    }

    #[test]
    fn project_notes_include_note_sessions_in_the_project() {
        // The file says /elsewhere, but its Session lives in project "p".
        let (fixture, note_id) = from_note();
        let listed = fixture
            .bridge("child")
            .call("list_notes", &json!({}))
            .unwrap();
        assert_eq!(listed["notes"][0]["id"], note_id.as_str());
        assert_eq!(listed["notes"][0]["session_id"], "s_note");
    }

    #[test]
    fn reports_to_a_parent_note_land_in_its_updates() {
        let (fixture, note_id) = from_note();
        let report = fixture
            .bridge("child")
            .call(
                "report_to_parent",
                &json!({"summary": "Fixed the flicker", "status": "done", "artifacts": ["PR #600"]}),
            )
            .unwrap();
        assert_eq!(report["recorded_in_note"], note_id.as_str());
        let source = fixture.store().load(&note_id).unwrap().to_markdown();
        assert!(source.contains("## Updates"), "{source}");
        assert!(
            source.contains("](diri://session/child)")
                && source.contains("done: Fixed the flicker (PR #600)"),
            "{source}"
        );
    }

    #[test]
    fn reports_from_a_todos_agent_land_under_that_todo() {
        let (fixture, note_id) = from_note();
        fixture
            .store()
            .update(&note_id, |note| {
                diri_notes::store::append_markdown(
                    note,
                    "- [ ] Draft the launch posts\n  - Tone: plain",
                );
                let index = handoff::find_todo(
                    note,
                    &handoff::TodoSelector::Text("Draft the launch posts".into()),
                )
                .expect("to-do");
                handoff::link_session(note, index, "@Codex", "child");
                Ok(())
            })
            .unwrap();
        let report = fixture
            .bridge("child")
            .call(
                "report_to_parent",
                &json!({"summary": "Drafted post 1 of 3", "status": "update"}),
            )
            .unwrap();
        assert!(report["todo"].is_u64(), "{report}");
        let source = fixture.store().load(&note_id).unwrap().to_markdown();
        assert!(
            source.contains(
                "- [ ] Draft the launch posts [@Codex](diri://session/child)\n  - Tone: plain\n  - [@"
            ) && source.contains("Drafted post 1 of 3"),
            "{source}"
        );
        assert!(!source.contains("## Updates"), "{source}");
    }

    #[test]
    fn a_note_is_not_an_agent_target_and_its_children_act_as_roots() {
        let (fixture, _) = from_note();
        let denied = fixture
            .bridge("child")
            .call(
                "send_prompt",
                &json!({"session_id": "s_note", "text": "hi"}),
            )
            .unwrap_err();
        assert!(denied.contains("is a note, not an agent"), "{denied}");
        // Started from a note, `child` is a root: it may add to any note.
        let other = fixture.note("Unrelated", None, "x");
        fixture
            .bridge("child")
            .call("write_note", &json!({"note": other, "append": "ok"}))
            .unwrap();
        // `grandchild` is delegated: its origin note yes, others no.
        fixture
            .bridge("grandchild")
            .call("write_note", &json!({"note": "origin", "append": "ok"}))
            .unwrap();
        let denied = fixture
            .bridge("grandchild")
            .call("write_note", &json!({"note": other, "append": "no"}))
            .unwrap_err();
        assert!(denied.contains("write_note denied"), "{denied}");
    }

    #[test]
    fn spawn_note_separates_unavailable_from_rejected() {
        let missing = Bridge::new(PathBuf::from("/nonexistent/diri.sock"), None);
        assert!(matches!(
            missing.spawn_note("/tmp", "t", "", None),
            Ok(NoteSpawn::Unavailable(_))
        ));

        let old = Peer::new(
            |_| json!({"__error": {"code": "not_found", "message": "no manifest for agent \"note\""}}),
        );
        let bridge = Bridge::new(old.path.clone(), None);
        assert!(matches!(
            bridge.spawn_note("/tmp", "t", "", None),
            Ok(NoteSpawn::Unavailable(reason)) if reason.contains("predates")
        ));

        let rejecting = Peer::new(
            |_| json!({"__error": {"code": "bad_request", "message": "cwd is not a directory"}}),
        );
        let bridge = Bridge::new(rejecting.path.clone(), None);
        assert!(bridge.spawn_note("/nope", "t", "", None).is_err());

        let created = note_session("s_new", "n-new", Some("parent"));
        let current = Peer::new(move |_| serde_json::to_value(&created).unwrap());
        let bridge = Bridge::new(current.path.clone(), None);
        let Ok(NoteSpawn::Created(record)) = bridge.spawn_note("/tmp", "t", "body", Some("parent"))
        else {
            panic!("expected a note session");
        };
        assert_eq!(record.note_id.as_deref(), Some("n-new"));

        let shell = super::super::tests::record("s_shell", None);
        let wrong = Peer::new(move |_| serde_json::to_value(&shell).unwrap());
        let bridge = Bridge::new(wrong.path.clone(), None);
        assert!(bridge.spawn_note("/tmp", "t", "", None).is_err());
    }
}
