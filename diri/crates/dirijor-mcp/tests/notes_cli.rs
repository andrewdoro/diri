#![cfg(unix)]
//! `dirijor note` against a real Engine: new notes become note Sessions (so
//! they reach the sidebar), and only a missing Engine falls back to a file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use diri_engine::{ControlServer, ManifestEngine, Registry};
use diri_proto::SessionRecord;

struct Server {
    registry: Arc<Mutex<Registry>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    socket: PathBuf,
}

impl Server {
    fn start(directory: &Path, notes: &Path) -> Self {
        let (engine, _) =
            ManifestEngine::load_dir(&diri_engine::detect::bundled_manifest_dir()).unwrap();
        let mut registry = Registry::new(Arc::new(engine), directory.join("state.json"));
        registry.load().unwrap();
        let registry = Arc::new(Mutex::new(registry));
        let socket = directory.join("engine.sock");
        let server = Arc::new(ControlServer::new(registry.clone(), &socket).with_notes_dir(notes));
        let listener = server.bind().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let server = server.clone();
                std::thread::spawn(move || {
                    let _ = server.serve(stream);
                });
            }
        });
        Self {
            registry,
            stop,
            thread: Some(thread),
            socket,
        }
    }

    fn notes(&self) -> Vec<SessionRecord> {
        self.registry
            .lock()
            .unwrap()
            .records()
            .into_iter()
            .filter(SessionRecord::is_note)
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn dirijor(
    socket: &Path,
    notes: &Path,
    cwd: &Path,
    caller: Option<&str>,
    words: &[&str],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dirijor"));
    command
        .args(words)
        .current_dir(cwd)
        .env_clear()
        .env("DIRIJOR_SOCKET", socket)
        .env("DIRI_NOTES_DIR", notes);
    if let Some(caller) = caller {
        command.env("DIRIJOR_SESSION_ID", caller);
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim()
        .to_owned()
}

struct Setup {
    _temp: tempfile::TempDir,
    notes: PathBuf,
    project: PathBuf,
}

fn setup() -> Setup {
    let temp = tempfile::tempdir().unwrap();
    let notes = temp.path().join("notes");
    let project = temp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let project = project.canonicalize().unwrap();
    Setup {
        notes,
        project,
        _temp: temp,
    }
}

#[test]
fn a_new_note_becomes_a_note_session_in_the_current_project() {
    let setup = setup();
    let server = Server::start(setup._temp.path(), &setup.notes);
    let id = stdout(&dirijor(
        &server.socket,
        &setup.notes,
        &setup.project,
        None,
        &["note", "add", "Launch plan\n\n- [ ] ship it"],
    ));

    let notes = server.notes();
    assert_eq!(notes.len(), 1);
    let record = &notes[0];
    assert_eq!(record.note_id.as_deref(), Some(id.as_str()));
    assert_eq!(record.title, "Launch plan");
    assert_eq!(record.cwd, setup.project.to_string_lossy());
    assert!(record.parent.is_none());
    let file = std::fs::read_to_string(setup.notes.join(format!("{id}.md"))).unwrap();
    assert!(file.contains("- [ ] ship it"), "{file}");
}

#[test]
fn an_agent_writing_a_note_becomes_its_parent_and_lends_its_project() {
    let setup = setup();
    let server = Server::start(setup._temp.path(), &setup.notes);
    let first = stdout(&dirijor(
        &server.socket,
        &setup.notes,
        &setup.project,
        None,
        &["note", "add", "PRD"],
    ));
    let parent = server.notes()[0].id.0.clone();

    // Written from somewhere else (a worktree) by the session `parent`.
    let elsewhere = setup._temp.path().canonicalize().unwrap();
    let second = stdout(&dirijor(
        &server.socket,
        &setup.notes,
        &elsewhere,
        Some(&parent),
        &["note", "add", "Findings"],
    ));
    assert_ne!(first, second);
    let child = server
        .notes()
        .into_iter()
        .find(|record| record.note_id.as_deref() == Some(second.as_str()))
        .unwrap();
    assert_eq!(
        child.parent.as_ref().map(|p| p.0.as_str()),
        Some(parent.as_str())
    );
    assert_eq!(child.cwd, setup.project.to_string_lossy());
}

#[test]
fn todos_collect_in_one_project_note_session() {
    let setup = setup();
    let server = Server::start(setup._temp.path(), &setup.notes);
    for item in ["one", "two"] {
        stdout(&dirijor(
            &server.socket,
            &setup.notes,
            &setup.project,
            None,
            &["note", "todo", item],
        ));
    }
    let notes = server.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].title, "To-dos");
    let id = notes[0].note_id.clone().unwrap();
    let file = std::fs::read_to_string(setup.notes.join(format!("{id}.md"))).unwrap();
    assert!(file.contains("- [ ] one\n- [ ] two"), "{file}");
}

#[test]
fn inbox_notes_and_a_missing_engine_write_files_only() {
    let setup = setup();
    let server = Server::start(setup._temp.path(), &setup.notes);
    stdout(&dirijor(
        &server.socket,
        &setup.notes,
        &setup.project,
        None,
        &["note", "add", "--inbox", "Loose thought"],
    ));
    assert!(server.notes().is_empty());

    let missing = setup._temp.path().join("no-engine.sock");
    let output = dirijor(
        &missing,
        &setup.notes,
        &setup.project,
        None,
        &["note", "add", "Offline"],
    );
    let id = stdout(&output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no sidebar entry yet"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let file = std::fs::read_to_string(setup.notes.join(format!("{id}.md"))).unwrap();
    assert!(
        file.contains(&format!("project: {}", setup.project.display())),
        "{file}"
    );
    assert!(server.notes().is_empty());
}
