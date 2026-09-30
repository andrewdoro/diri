//! Diri Notes for agents: discover, read, and add to the user's notes.
//!
//! Notes live outside project repos, in Diri's app-support directory, so
//! agents find them here rather than on disk: the notes for their project,
//! the notes that mention them, and (once notes are sessions) the note they
//! were started from. Reads need no Engine; anything about sessions (who is
//! mentioned, their status, write authorization) comes from `session.list`.

use super::*;
use diri_notes::handoff::{self, TodoSelector};
use diri_notes::mentions::{self, MentionTarget};
use diri_notes::store::{self as note_store, Note, NoteMeta, NoteStore, Resolve};

const DEFAULT_NOTE_LIMIT: u64 = 50;
const MAX_NOTE_LIMIT: u64 = 200;
const MAX_APPEND_BYTES: usize = 64 * 1024;
const ORIGIN: &str = "origin";

impl Bridge {
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

        let snapshot = if project == "mine" || mentions.is_some() {
            Some(self.snapshot()?)
        } else {
            None
        };
        let project_root = match project.as_str() {
            "all" => None,
            "mine" => Some(self.caller_project_root(snapshot.as_ref().expect("fetched above"))?),
            path => Some(path.to_owned()),
        };
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
                let sessions = &snapshot.as_ref().expect("fetched above").sessions;
                let lineage = Lineage::new(sessions, Some(&subject));
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
            if note.archived && !include_archived {
                continue;
            }
            if let Some(root) = &project_root
                && !note
                    .project
                    .as_deref()
                    .is_some_and(|p| policy::same_path(p, root))
            {
                continue;
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
        let meta = self.resolve_note(&store, &required_string(args, "note")?)?;
        let note = store
            .load(&meta.id)
            .map_err(|e| format!("cannot read note {}: {e}", meta.id))?;
        // Notes are readable without Diri running; session facts are extra.
        let sessions = self.sessions();
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
        let mentions: Vec<Value> = mentions::mentions(&note.doc)
            .into_iter()
            .map(|mention| {
                let mut value = json!({"label": mention.label, "block": mention.block});
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
        let meta = self.resolve_note(&store, &required_string(args, "note")?)?;
        let snapshot = self.snapshot()?;
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
        })?;
        let link = match link {
            Some(id) => {
                let record = find_session(&snapshot.sessions, &id)?;
                let label = format!(
                    "{}: {}",
                    short_label(record.effective_kind().id()),
                    record.title
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

    fn resolve_note(&self, store: &NoteStore, query: &str) -> Result<NoteMeta, String> {
        if query == ORIGIN {
            // The origin is the nearest note session among the caller's
            // ancestors; it arrives with note sessions on `feat/notes`.
            return Err(
                "\"origin\" needs note sessions, which this Diri build does not have yet; pass the note's id or title (list_notes mentions:\"me\" finds notes that mention you)"
                    .into(),
            );
        }
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

    fn caller_project_root(&self, snapshot: &SessionListResult) -> Result<String, String> {
        let caller = self.require_caller()?;
        let record = find_session(&snapshot.sessions, caller)?;
        snapshot
            .projects
            .iter()
            .find(|project| project.id == record.project_id)
            .map(|project| project.root.clone())
            .ok_or_else(|| {
                format!(
                    "calling session {caller} has no live project; pass project:\"all\" or a path"
                )
            })
    }
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
        assert!(origin.contains("note sessions"), "{origin}");
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
}
