//! Opt-in: the real Antigravity CLI (`agy`) driven through a private Engine.
//!
//! No Google account is needed. agy 1.2.17 runs against the Gemini API with
//! `modelProvider: "gemini"` in its settings, a dummy `GEMINI_API_KEY`, and
//! `GOOGLE_GEMINI_BASE_URL` pointed at `fixtures/fake_antigravity_api.py` on
//! 127.0.0.1, which scripts slow streams, a command that needs approval and
//! an `ask_question` modal. HOME, project and Engine socket live in a temp
//! dir removed on drop, and auto-update is off; the developer's `~/.gemini`
//! is never read or written.
//!
//! `DIRI_ANTIGRAVITY_BIN_DIR` must hold `agy`; Python 3 must be on PATH.
//! The installer takes a directory, so nothing lands in `~/.local/bin`:
//!
//! ```sh
//! curl -fsSL https://antigravity.google/cli/install.sh -o /tmp/agy-install.sh
//! HOME=/tmp/agy-home bash /tmp/agy-install.sh --dir /tmp/agy-bin
//! DIRI_ANTIGRAVITY_BIN_DIR=/tmp/agy-bin \
//!   cargo test -p diri-engine --test antigravity_real -- --ignored --nocapture --test-threads=1
//! ```
//!
//! With `DIRI_ANTIGRAVITY_SCREENS=<dir>` every named state's screen is also
//! written there; `fixtures/antigravity_screens` came from such a run.
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

    fn record(&mut self, id: &str) -> Value {
        let list = self.call("session.list", json!({})).unwrap();
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"] == id)
            .cloned()
            .unwrap_or_default()
    }

    fn status(&mut self, id: &str) -> String {
        self.record(id)["status"].to_string()
    }

    fn screen(&mut self, id: &str) -> String {
        self.call("session.read_screen", json!({ "sessionID": id }))
            .map(|result| result["text"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default()
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
                println!("[{label} final] {status}\n{}", indent(&screen));
                return seen;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Writes the current screen to `$DIRI_ANTIGRAVITY_SCREENS/<name>.txt`.
    fn capture(&mut self, id: &str, name: &str) {
        let Some(dir) = std::env::var_os("DIRI_ANTIGRAVITY_SCREENS") else {
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
    api: Child,
}

impl Fixture {
    fn requests(&self) -> String {
        std::fs::read_to_string(self.temp.path().join("api.log")).unwrap_or_default()
    }

    /// The main model's requests; agy also asks flash-lite for titles.
    fn turns(&self) -> Vec<String> {
        self.requests()
            .lines()
            .filter(|line| line.starts_with("POST ") && !line.contains("flash-lite"))
            .map(str::to_owned)
            .collect()
    }

    fn settings(&self, settings: Value) {
        std::fs::write(
            self.home.join(".gemini/antigravity-cli/settings.json"),
            serde_json::to_vec_pretty(&settings).unwrap(),
        )
        .unwrap();
    }

    fn trust_project(&self) {
        self.settings(json!({
            "modelProvider": "gemini",
            "trustedWorkspaces": [self.project.canonicalize().unwrap()],
        }));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.api.kill();
        let _ = self.api.wait();
    }
}

/// A private HOME (onboarding done, Gemini API key mode, project untrusted)
/// and project, with the Engine's inherited environment pointed at them and
/// at the agy under test. With `signed_in` false agy has no credential and
/// stops at its login screen. None when not opted in.
fn fixture(signed_in: bool) -> Option<Fixture> {
    let Some(bin) = std::env::var_os("DIRI_ANTIGRAVITY_BIN_DIR") else {
        eprintln!("DIRI_ANTIGRAVITY_BIN_DIR unset; skipping");
        return None;
    };
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    let state = home.join(".gemini/antigravity-cli");
    std::fs::create_dir_all(state.join("cache")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    // The colour-scheme and terms pages: answered once per HOME.
    std::fs::write(
        state.join("cache/onboarding.json"),
        json!({
            "consumerOnboardingComplete": true,
            "enterpriseOnboardingComplete": false,
            "onboardingComplete": true,
        })
        .to_string(),
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let api = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_antigravity_api.py"))
        .arg(port.to_string())
        .arg(temp.path().join("api.log"))
        .arg(project.canonicalize().unwrap())
        .spawn()
        .expect("python3 for the fake Gemini API");
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "fake Gemini API never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    let path = format!(
        "{}:{}",
        Path::new(&bin).display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: --test-threads=1, and set before the Engine spawns anything.
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("HOME", &home);
        std::env::set_var("AGY_CLI_DISABLE_AUTO_UPDATE", "1");
        for name in [
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GEMINI_BASE_URL",
            "AGY_ADC_AUTH",
            "GOOGLE_APPLICATION_CREDENTIALS",
        ] {
            std::env::remove_var(name);
        }
        if signed_in {
            std::env::set_var("GEMINI_API_KEY", "fake-key-for-diri-e2e");
            std::env::set_var("GOOGLE_GEMINI_BASE_URL", format!("http://127.0.0.1:{port}"));
        }
    }
    let fixture = Fixture {
        temp,
        home,
        project,
        api,
    };
    if signed_in {
        fixture.settings(json!({ "modelProvider": "gemini" }));
    }
    Some(fixture)
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
    let mut params = json!({
        "kind": { "antigravity": {} },
        "cwd": project,
        "initialCols": 110,
        "initialRows": 36,
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

/// Records a failure unless the last status a watch saw was idle.
fn settled(failures: &mut Vec<String>, what: &str, seen: &[String]) {
    if !seen.last().is_some_and(|status| status.contains("idle")) {
        failures.push(format!("{what} never settled idle: {seen:?}"));
    }
}

fn manifest_action(name: &str) -> Option<Value> {
    let manifest: Value =
        serde_json::from_str(include_str!("../manifests/antigravity.json")).unwrap();
    manifest["agent"].get(name).cloned()
}

/// Sends the manifest's Approve or Deny action the way the app does; a
/// missing action is a failure.
fn act(client: &mut Client, id: &str, name: &str, failures: &mut Vec<String>) {
    let Some(action) = manifest_action(name) else {
        failures.push(format!("Antigravity has no manifest {name} action"));
        return;
    };
    client
        .call(
            "session.send_text",
            json!({ "sessionID": id, "text": action["text"], "submit": action["submit"] }),
        )
        .expect("manifest action");
}

/// An initial prompt into a folder agy has never trusted, a streamed turn, a
/// command denied then approved, a file write, a question, then kill and
/// resume.
#[test]
#[ignore = "needs DIRI_ANTIGRAVITY_BIN_DIR and a real agy"]
fn prompts_stream_ask_permission_and_resume() {
    let Some(fixture) = fixture(true) else {
        return;
    };
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
    let seen = client.watch(&id, "initial", Duration::from_secs(30), |status, screen| {
        status.contains("idle") && screen.contains("  PINEAPPLE")
    });
    settled(&mut failures, "the initial turn", &seen);
    client.capture(&id, "done");
    if !fixture
        .turns()
        .iter()
        .any(|line| line.contains("last_user='Reply with the word PINEAPPLE please.'"))
    {
        failures.push(format!(
            "the initial prompt never reached the API:\n{}",
            fixture.requests()
        ));
    }

    client.send(&id, "SLOW stream something");
    let seen = client.watch(&id, "stream", Duration::from_secs(20), |status, screen| {
        status.contains("idle") && screen.contains("SLOWDONE")
    });
    if !seen.iter().any(|status| status.contains("working")) {
        failures.push(format!("a streamed turn never read as working: {seen:?}"));
    }
    settled(&mut failures, "the streamed turn", &seen);
    client.capture(&id, "idle");

    // Working, sampled mid-stream on its own turn so the fixture is stable.
    client.send(&id, "SLOW again");
    client.watch(&id, "stream-2", Duration::from_secs(10), |_, screen| {
        screen.contains("Generating...")
    });
    client.capture(&id, "working");
    client.watch(
        &id,
        "stream-2-done",
        Duration::from_secs(20),
        |status, screen| status.contains("idle") && screen.matches("SLOWDONE").count() >= 2,
    );

    client.send(&id, "RUNCMD and deny");
    let seen = client.watch(&id, "permission", Duration::from_secs(20), |status, _| {
        status.contains("needsInput")
    });
    if !seen.iter().any(|status| status.contains("permission")) {
        failures.push(format!(
            "the command approval never read as a permission: {seen:?}"
        ));
    }
    client.capture(&id, "permission_command");
    act(&mut client, &id, "deny", &mut failures);
    let seen = client.watch(&id, "denied", Duration::from_secs(20), |status, screen| {
        status.contains("idle") && screen.contains("Interrupted")
    });
    settled(&mut failures, "the denied command", &seen);
    client.capture(&id, "denied");
    if fixture.project.join("diri-e2e-file").exists() {
        failures.push("denying still ran the command".into());
    }

    client.send(&id, "RUNCMD for me");
    let seen = client.watch(&id, "permission-2", Duration::from_secs(20), |status, _| {
        status.contains("needsInput")
    });
    if !seen.iter().any(|status| status.contains("permission")) {
        failures.push(format!(
            "the second command approval never read as a permission: {seen:?}"
        ));
    }
    act(&mut client, &id, "approve", &mut failures);
    let seen = client.watch(
        &id,
        "approved",
        Duration::from_secs(20),
        |status, screen| status.contains("idle") && screen.contains("DONECMD"),
    );
    settled(&mut failures, "the approved turn", &seen);
    if !fixture.project.join("diri-e2e-file").exists() {
        failures.push("approving did not run the command".into());
    }

    client.send(&id, "WRITEFILE please");
    let seen = client.watch(&id, "write", Duration::from_secs(20), |status, _| {
        status.contains("needsInput")
    });
    if !seen.iter().any(|status| status.contains("permission")) {
        failures.push(format!(
            "the file write never read as a permission: {seen:?}"
        ));
    }
    client.capture(&id, "permission_file");
    act(&mut client, &id, "approve", &mut failures);
    let seen = client.watch(&id, "written", Duration::from_secs(20), |status, screen| {
        status.contains("idle") && screen.matches("DONECMD").count() >= 2
    });
    settled(&mut failures, "the approved file write", &seen);
    if !fixture.project.join("diri-e2e-notes.txt").exists() {
        failures.push("approving did not write the file".into());
    }

    client.send(&id, "ASKQ something");
    let seen = client.watch(&id, "question", Duration::from_secs(20), |status, _| {
        status.contains("needsInput")
    });
    if !seen.iter().any(|status| status.contains("question")) {
        failures.push(format!("ask_question never read as a question: {seen:?}"));
    }
    client.capture(&id, "question");
    // Enter picks the preselected first option.
    client.send(&id, "");
    let seen = client.watch(
        &id,
        "answered",
        Duration::from_secs(20),
        |status, screen| status.contains("idle") && screen.matches("DONECMD").count() >= 3,
    );
    settled(&mut failures, "the answered question", &seen);

    let _ = client.call("session.kill", json!({ "sessionID": id }));
    client.watch(&id, "killed", Duration::from_secs(5), |status, _| {
        status.contains("exited")
    });
    let record = client.record(&id);
    if record["resumability"] != "resumable" {
        failures.push(format!(
            "an exited Antigravity is not resumable: {}",
            record["resumability"]
        ));
    }
    if let Err(error) = client.call("session.resume", json!({ "sessionID": id })) {
        failures.push(format!("resume: {error}"));
    }
    let seen = client.watch(&id, "resumed", Duration::from_secs(30), |status, screen| {
        status.contains("idle") && screen.contains("DONECMD")
    });
    settled(&mut failures, "the resumed session", &seen);
    client.send(&id, "Say MANGO now");
    let seen = client.watch(
        &id,
        "after-resume",
        Duration::from_secs(20),
        |status, screen| status.contains("idle") && screen.contains("  MANGO"),
    );
    settled(&mut failures, "the resumed follow-up", &seen);
    let turns = fixture.turns();
    for prompt in [
        "Reply with the word PINEAPPLE please.",
        "SLOW stream something",
        "RUNCMD and deny",
        "RUNCMD for me",
        "WRITEFILE please",
        "ASKQ something",
        "Say MANGO now",
    ] {
        if !turns
            .iter()
            .any(|line| line.contains(&format!("last_user='{prompt}'")))
        {
            failures.push(format!("{prompt} never reached the API:\n{turns:#?}"));
        }
    }
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    println!("requests:\n{}", fixture.requests());
    assert!(
        failures.is_empty(),
        "failures:\n  {}",
        failures.join("\n  ")
    );
}

/// Without a prompt the trust dialog is the user's: it reads as a question,
/// and answering it lands on the idle composer.
#[test]
#[ignore = "needs DIRI_ANTIGRAVITY_BIN_DIR and a real agy"]
fn first_run_trust_needs_input_then_idles() {
    let Some(fixture) = fixture(true) else {
        return;
    };
    let mut client = start(fixture.temp.path());
    let (id, _) = spawn(&mut client, &fixture.project, None);
    client.watch(&id, "trust", Duration::from_secs(30), |status, screen| {
        status.contains("needsInput") && screen.contains("Do you trust the contents")
    });
    client.capture(&id, "trust");
    let trust_status = client.status(&id);
    client.send(&id, "");
    client.watch(&id, "idle", Duration::from_secs(30), |status, screen| {
        status.contains("idle") && screen.contains("? for shortcuts")
    });
    client.capture(&id, "fresh");
    let idle_status = client.status(&id);
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(trust_status.contains("needsInput"), "trust: {trust_status}");
    assert!(idle_status.contains("idle"), "idle: {idle_status}");
}

/// Signed out, agy stops at its login method picker after a "Signing in..."
/// spinner. That is a question for the user, not a turn in progress, and an
/// initial prompt must not be typed into it.
#[test]
#[ignore = "needs DIRI_ANTIGRAVITY_BIN_DIR and a real agy"]
fn the_login_screen_needs_input() {
    let Some(fixture) = fixture(false) else {
        return;
    };
    let mut client = start(fixture.temp.path());
    let (id, delivered) = spawn(&mut client, &fixture.project, Some("Say KIWI"));
    let seen = client.watch(&id, "login", Duration::from_secs(30), |status, screen| {
        status.contains("needsInput") && screen.contains("Select login method:")
    });
    client.capture(&id, "login");
    let screen = client.screen(&id);
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(
        screen.contains("Select login method:"),
        "agy did not show its login picker; the scenario is stale"
    );
    assert!(
        seen.last()
            .is_some_and(|status| status.contains("needsInput")),
        "the login picker never read as needing input: {seen:?}"
    );
    assert!(
        delivered.is_err(),
        "a prompt typed into the login picker was reported delivered"
    );
    assert!(!screen.contains("KIWI"), "the prompt was typed into login");
}

/// Quitting agy lands in the login shell in the same tab (the session stays
/// alive and takes shell input), never closes it.
#[test]
#[ignore = "needs DIRI_ANTIGRAVITY_BIN_DIR and a real agy"]
fn quitting_returns_to_the_login_shell() {
    let Some(fixture) = fixture(true) else {
        return;
    };
    fixture.trust_project();
    let mut client = start(fixture.temp.path());
    let (id, _) = spawn(&mut client, &fixture.project, None);
    client.watch(&id, "idle", Duration::from_secs(30), |status, screen| {
        status.contains("idle") && screen.contains("? for shortcuts")
    });
    client.send(&id, "/exit");
    client.watch(&id, "quit", Duration::from_secs(20), |_, screen| {
        screen.contains("agy --conversation=")
    });
    std::thread::sleep(Duration::from_millis(1500));
    client.send(&id, "echo DIRI_SHELL_$((40 + 2))");
    let seen = client.watch(&id, "shell", Duration::from_secs(20), |_, screen| {
        screen.contains("DIRI_SHELL_42")
    });
    let screen = client.screen(&id);
    let status = client.status(&id);
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(
        screen.contains("DIRI_SHELL_42"),
        "the tab did not take shell input after agy quit: {seen:?}\n{screen}"
    );
    assert!(!status.contains("exited"), "the session ended: {status}");
}
