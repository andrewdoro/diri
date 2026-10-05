//! Opt-in: the real Gemini CLI driven through a private Engine.
//!
//! Nothing here needs a Google account. The CLI talks to
//! `fixtures/fake_gemini_api.py` (spawned on a free 127.0.0.1 port through
//! `GOOGLE_GEMINI_BASE_URL`), which scripts slow streams and a shell tool call
//! from keywords in the prompt. HOME, the project and the Engine socket all
//! live in a temp dir removed on drop; the developer's `~/.gemini` is never
//! read or written.
//!
//! `DIRI_GEMINI_BIN_DIR` must hold a `gemini` executable, and `node` and
//! `python3` must be on PATH:
//!
//! ```sh
//! npm install --prefix /tmp/gem @google/gemini-cli
//! DIRI_GEMINI_BIN_DIR=/tmp/gem/node_modules/.bin \
//!   cargo test -p diri-engine --test gemini_real -- --ignored --nocapture --test-threads=1
//! ```
//!
//! With `DIRI_GEMINI_SCREENS=<dir>` the named states' screens are written
//! there; `fixtures/gemini_screens` came from such a run.
//!
//! The tests set process-wide environment the Engine hands to its children,
//! so they must run with `--test-threads=1`.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use diri_engine::control::ControlServer;
use diri_engine::detect::ManifestEngine;
use diri_engine::registry::Registry;
use diri_proto::ControlMessage;
use serde_json::{Value, json};

struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Client {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let message = ControlMessage::Request {
            id: self.next,
            method: method.into(),
            params: Some(params),
        };
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        self.writer.write_all(&bytes).unwrap();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            if let ControlMessage::Response { id, result, .. } =
                serde_json::from_str::<ControlMessage>(&line).unwrap()
                && id == self.next
            {
                return result.map_err(|error| format!("{error:?}"));
            }
        }
    }

    fn status(&mut self, id: &str) -> String {
        let list = self.call("session.list", json!({})).unwrap();
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"] == id)
            .map(|record| record["status"].to_string())
            .unwrap_or_default()
    }

    fn screen(&mut self, id: &str) -> String {
        self.call("session.read_screen", json!({ "sessionID": id }))
            .map(|result| result["text"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    }

    /// Writes the current screen to `$DIRI_GEMINI_SCREENS/<name>.txt`.
    fn capture(&mut self, id: &str, name: &str) {
        let Some(dir) = std::env::var_os("DIRI_GEMINI_SCREENS") else {
            return;
        };
        let screen = self.screen(id);
        let trimmed = screen
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join(format!("{name}.txt")),
            format!("{}\n", trimmed.trim_end()),
        )
        .unwrap();
    }

    fn send(&mut self, id: &str, text: &str) {
        self.call(
            "session.send_text",
            json!({ "sessionID": id, "text": text, "submit": true }),
        )
        .expect("send_text");
    }

    /// Samples status every 100 ms until `done` holds or `within` passes,
    /// printing each transition with its screen. Returns every status seen.
    fn watch(
        &mut self,
        id: &str,
        label: &str,
        within: Duration,
        mut done: impl FnMut(&str, &str) -> bool,
    ) -> Vec<String> {
        let start = Instant::now();
        let mut seen: Vec<String> = Vec::new();
        loop {
            let status = self.status(id);
            let screen = self.screen(id);
            if seen.last() != Some(&status) {
                println!(
                    "[{label} +{}ms] {status}\n{}",
                    start.elapsed().as_millis(),
                    indent(&screen)
                );
                seen.push(status.clone());
            }
            if done(&status, &screen) || start.elapsed() > within {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn indent(screen: &str) -> String {
    screen
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("    | {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

struct Fixture {
    temp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    api: Option<Child>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(api) = &mut self.api {
            let _ = api.kill();
            let _ = api.wait();
        }
    }
}

/// A private HOME and project, with the Engine's inherited environment
/// pointed at them and at the Gemini under test. None when not opted in.
fn fixture(settings: Option<&str>, fake_api: bool) -> Option<Fixture> {
    let Some(bin) = std::env::var_os("DIRI_GEMINI_BIN_DIR") else {
        eprintln!("DIRI_GEMINI_BIN_DIR unset; skipping");
        return None;
    };
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    if let Some(settings) = settings {
        std::fs::write(home.join(".gemini/settings.json"), settings).unwrap();
    }
    let api = fake_api.then(|| {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new("python3")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_gemini_api.py"))
            .arg(port.to_string())
            .arg(temp.path().join("api.log"))
            .spawn()
            .expect("python3 for the fake Gemini API");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "fake Gemini API never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        (child, port)
    });
    let path = format!(
        "{}:{}",
        Path::new(&bin).display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: --test-threads=1, and set before the Engine spawns anything.
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("HOME", &home);
        for name in ["GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_GEMINI_BASE_URL"] {
            std::env::remove_var(name);
        }
        if let Some((_, port)) = &api {
            std::env::set_var("GEMINI_API_KEY", "fake-key-for-diri-e2e");
            std::env::set_var("GOOGLE_GEMINI_BASE_URL", format!("http://127.0.0.1:{port}"));
        }
    }
    Some(Fixture {
        temp,
        home,
        project,
        api: api.map(|(child, _)| child),
    })
}

fn start(temp: &Path) -> Client {
    let dir = diri_engine::detect::bundled_manifest_dir()
        .canonicalize()
        .unwrap();
    let (engine, _) = ManifestEngine::load_dir(&dir).unwrap();
    let registry = Arc::new(Mutex::new(Registry::new(
        Arc::new(engine),
        temp.join("state.json"),
    )));
    let server = Arc::new(
        ControlServer::new(Arc::clone(&registry), temp.join("daemon.sock"))
            .with_logs_dir(temp.join("logs")),
    );
    let listener = server.bind().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let server = Arc::clone(&server);
            std::thread::spawn(move || {
                let _ = server.serve(stream);
            });
        }
    });
    let stream = UnixStream::connect(temp.join("daemon.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    Client {
        writer: stream.try_clone().unwrap(),
        reader: BufReader::new(stream),
        next: 0,
    }
}

fn spawn(
    client: &mut Client,
    project: &Path,
    prompt: Option<&str>,
) -> (String, Result<(), String>) {
    spawn_sized(client, project, prompt, (100, 30))
}

fn spawn_sized(
    client: &mut Client,
    project: &Path,
    prompt: Option<&str>,
    (cols, rows): (u16, u16),
) -> (String, Result<(), String>) {
    let mut params = json!({
        "kind": { "gemini": {} },
        "cwd": project,
        "initialCols": cols,
        "initialRows": rows,
    });
    if let Some(prompt) = prompt {
        params["initialPrompt"] = json!(prompt);
    }
    match client.call("session.spawn", params) {
        Ok(record) => (record["id"].as_str().unwrap().to_string(), Ok(())),
        Err(error) => {
            let list = client.call("session.list", json!({})).unwrap();
            let id = list["sessions"][0]["id"].as_str().unwrap().to_string();
            (id, Err(error))
        }
    }
}

const API_KEY_SETTINGS: &str = r#"{"security":{"auth":{"selectedType":"gemini-api-key"}},"model":{"name":"gemini-2.5-flash"}}"#;

/// Gemini's first launch in a folder asks whether to trust it. That is a
/// question for the user, not a session still starting.
#[test]
#[ignore = "needs DIRI_GEMINI_BIN_DIR and a real Gemini CLI"]
fn the_folder_trust_dialog_needs_input() {
    let Some(fixture) = fixture(Some(API_KEY_SETTINGS), true) else {
        return;
    };
    let mut client = start(fixture.temp.path());
    let (id, _) = spawn(&mut client, &fixture.project, None);
    let seen = client.watch(&id, "trust", Duration::from_secs(25), |status, _| {
        status.contains("needsInput")
    });
    let screen = client.screen(&id);
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(
        screen.contains("Do you trust the files in this folder?"),
        "Gemini did not show its trust dialog; the scenario is stale"
    );
    assert!(
        seen.last()
            .is_some_and(|status| status.contains("needsInput")),
        "the trust dialog never read as needing input: {seen:?}"
    );
}

/// An initial prompt into an untrusted folder: the trust dialog is answered
/// and the prompt still reaches Gemini after the restart that applies it.
#[test]
#[ignore = "needs DIRI_GEMINI_BIN_DIR and a real Gemini CLI"]
fn an_initial_prompt_survives_the_trust_restart() {
    let Some(fixture) = fixture(Some(API_KEY_SETTINGS), true) else {
        return;
    };
    let mut client = start(fixture.temp.path());
    let (id, delivered) = spawn(
        &mut client,
        &fixture.project,
        Some("Reply with the word PINEAPPLE please."),
    );
    client.watch(
        &id,
        "untrusted",
        Duration::from_secs(15),
        |status, screen| status.contains("idle") && screen.contains("✦ PINEAPPLE"),
    );
    let requests = std::fs::read_to_string(fixture.temp.path().join("api.log")).unwrap_or_default();
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(delivered.is_ok(), "initial prompt: {delivered:?}");
    assert!(
        requests.contains("PINEAPPLE"),
        "the prompt never reached Gemini:\n{requests}"
    );
}

/// A trusted folder: initial prompt, a streamed turn, a follow-up that needs
/// shell permission, approval the way the app sends it, then kill and resume.
#[test]
#[ignore = "needs DIRI_GEMINI_BIN_DIR and a real Gemini CLI"]
fn prompts_stream_ask_permission_and_resume() {
    let Some(fixture) = fixture(Some(API_KEY_SETTINGS), true) else {
        return;
    };
    std::fs::write(
        fixture.home.join(".gemini/trustedFolders.json"),
        json!({ fixture.project.canonicalize().unwrap().to_string_lossy(): "TRUST_FOLDER" })
            .to_string(),
    )
    .unwrap();
    let mut client = start(fixture.temp.path());
    let mut failures = Vec::new();

    let (id, delivered) = spawn(
        &mut client,
        &fixture.project,
        Some("Reply with the word PINEAPPLE please."),
    );
    if let Err(error) = delivered {
        failures.push(format!("initial prompt: {error}"));
    }
    client.watch(&id, "initial", Duration::from_secs(10), |status, screen| {
        status.contains("idle") && screen.contains("✦ PINEAPPLE")
    });
    if !client.screen(&id).contains("✦ PINEAPPLE") {
        failures.push("Gemini never answered the initial prompt".into());
    }

    client.send(&id, "SLOW stream something");
    let seen = client.watch(&id, "stream", Duration::from_secs(15), |status, screen| {
        status.contains("idle") && screen.contains("SLOWDONE")
    });
    if !seen.iter().any(|status| status.contains("working")) {
        failures.push(format!("a streamed turn never read as working: {seen:?}"));
    }
    if !client.screen(&id).contains("SLOWDONE") {
        failures.push("the streamed turn never finished".into());
    }

    client.send(&id, "RUNCMD for me");
    let seen = client.watch(&id, "permission", Duration::from_secs(10), |status, _| {
        status.contains("needsInput")
    });
    if !seen.iter().any(|status| status.contains("permission")) {
        failures.push(format!(
            "the shell confirmation never read as a permission: {seen:?}"
        ));
    }
    let requests = std::fs::read_to_string(fixture.temp.path().join("api.log")).unwrap_or_default();
    if !requests.contains("last_user='RUNCMD for me'") {
        failures.push(format!(
            "the follow-up did not arrive on its own:\n{requests}"
        ));
    }
    // The app's Approve for gemini: a bare Enter on "1. Allow once".
    client.send(&id, "");
    client.watch(
        &id,
        "approved",
        Duration::from_secs(10),
        |status, screen| status.contains("idle") && screen.contains("DONECMD"),
    );
    if !fixture.project.join("diri-e2e-file").exists() {
        failures.push("approving did not run the shell command".into());
    }

    let _ = client.call("session.kill", json!({ "sessionID": id }));
    client.watch(&id, "killed", Duration::from_secs(5), |status, _| {
        status.contains("exited")
    });
    if let Err(error) = client.call("session.resume", json!({ "sessionID": id })) {
        failures.push(format!("resume: {error}"));
    }
    client.watch(&id, "resumed", Duration::from_secs(15), |status, screen| {
        status.contains("idle") && screen.contains("DONECMD")
    });
    if !client.screen(&id).contains("DONECMD") {
        failures.push("the resumed tab does not show the earlier conversation".into());
    }
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(
        failures.is_empty(),
        "failures:\n  {}",
        failures.join("\n  ")
    );
}

fn trust(fixture: &Fixture) {
    std::fs::write(
        fixture.home.join(".gemini/trustedFolders.json"),
        json!({ fixture.project.canonicalize().unwrap().to_string_lossy(): "TRUST_FOLDER" })
            .to_string(),
    )
    .unwrap();
}

/// Signed out, Gemini opens "How would you like to authenticate?" in place
/// of its composer. That is the user's question: it reads as needing input,
/// and an initial prompt is not typed into it, where its Enter would pick
/// "Sign in with Google" and open a browser.
#[test]
#[ignore = "needs DIRI_GEMINI_BIN_DIR and a real Gemini CLI"]
fn signed_out_auth_dialog_needs_input() {
    let Some(fixture) = fixture(None, false) else {
        return;
    };
    trust(&fixture);
    let mut client = start(fixture.temp.path());
    let (id, delivered) = spawn(&mut client, &fixture.project, Some("Say KIWI"));
    let seen = client.watch(&id, "auth", Duration::from_secs(25), |status, screen| {
        status.contains("needsInput") && screen.contains("How would you like to authenticate")
    });
    client.capture(&id, "auth");
    let screen = client.screen(&id);
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(
        screen.contains("How would you like to authenticate"),
        "Gemini did not show its auth dialog; the scenario is stale"
    );
    assert!(
        seen.last()
            .is_some_and(|status| status.contains("needsInput")),
        "the auth dialog never read as needing input: {seen:?}"
    );
    assert!(
        delivered.is_err(),
        "a prompt typed into the auth dialog was reported delivered"
    );
    assert!(
        !screen.contains("Waiting for authentication"),
        "the injector's Enter started Google sign-in"
    );
}

/// A long multi-line prompt (an orchestrator's brief) into an 80x24 tab, the
/// size a spawned child starts at: it is submitted as one turn, not left in
/// the composer.
#[test]
#[ignore = "needs DIRI_GEMINI_BIN_DIR and a real Gemini CLI"]
fn a_long_initial_prompt_is_submitted_at_80_by_24() {
    let Some(fixture) = fixture(Some(API_KEY_SETTINGS), true) else {
        return;
    };
    trust(&fixture);
    let mut client = start(fixture.temp.path());
    let mut prompt = String::new();
    for paragraph in 0..16 {
        prompt.push_str(&format!(
            "Paragraph {paragraph}: read the shared brief, keep the change small, \
             match the surrounding style and report what you verified.\n\n"
        ));
        prompt.push_str("- one bullet\n- another bullet with `code`\n\n");
    }
    prompt.push_str("Finally reply with the word GUAVA.");
    assert!(prompt.len() > 2000);
    let (id, delivered) = spawn_sized(&mut client, &fixture.project, Some(&prompt), (80, 24));
    let seen = client.watch(&id, "long", Duration::from_secs(30), |status, screen| {
        status.contains("idle") && screen.contains("✦ GUAVA")
    });
    let screen = client.screen(&id);
    let requests = std::fs::read_to_string(fixture.temp.path().join("api.log")).unwrap_or_default();
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(delivered.is_ok(), "initial prompt: {delivered:?}");
    assert!(
        screen.contains("✦ GUAVA"),
        "the long prompt was never answered: {seen:?}\n{screen}"
    );
    // The log keeps 80 characters of the prompt: one turn, not one per line.
    assert_eq!(
        requests.matches("last_user='Paragraph 0:").count(),
        1,
        "the long prompt was not sent as one turn:\n{requests}"
    );
}

/// Quitting Gemini lands in the login shell in the same tab, which keeps
/// taking input; the tab is not closed.
#[test]
#[ignore = "needs DIRI_GEMINI_BIN_DIR and a real Gemini CLI"]
fn quitting_returns_to_the_login_shell() {
    let Some(fixture) = fixture(Some(API_KEY_SETTINGS), true) else {
        return;
    };
    trust(&fixture);
    let mut client = start(fixture.temp.path());
    let (id, _) = spawn(&mut client, &fixture.project, None);
    client.watch(&id, "idle", Duration::from_secs(20), |status, _| {
        status.contains("idle")
    });
    client.capture(&id, "idle_fresh");
    client.send(&id, "/quit");
    std::thread::sleep(Duration::from_secs(3));
    client.send(&id, "echo DIRI_SHELL_$((40 + 2))");
    let seen = client.watch(&id, "shell", Duration::from_secs(20), |_, screen| {
        screen.contains("DIRI_SHELL_42")
    });
    let screen = client.screen(&id);
    let status = client.status(&id);
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(
        screen.contains("DIRI_SHELL_42"),
        "the tab did not take shell input after Gemini quit: {seen:?}\n{screen}"
    );
    assert!(!status.contains("exited"), "the session ended: {status}");
}
