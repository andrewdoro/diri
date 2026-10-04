//! Opt-in: the real Droid TUI driven through a private Engine.
//!
//! Droid 0.233.0 runs in airgap mode (no Factory login) with one BYOK
//! custom model, an Anthropic-format endpoint served by
//! `fixtures/fake_droid_api.py` on 127.0.0.1. The fixture scripts slow
//! streams, a shell command that needs approval, and an AskUser question;
//! no account or real key is needed. HOME, project, and Engine socket are
//! temporary; auto-update is off. The fake API also refuses outbound
//! HTTP(S) proxy requests.
//!
//! `DIRI_DROID_BIN_DIR` must hold `droid`; Python 3 must be on PATH:
//!
//! ```sh
//! npm install --prefix /tmp/droid @factory/cli@0.233.0
//! DIRI_DROID_BIN_DIR=/tmp/droid/node_modules/.bin \
//!   cargo test -p diri-engine --test droid_real -- --ignored --nocapture --test-threads=1
//! ```
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
        let mut sampled_stream = false;
        loop {
            let status = self.status(id);
            let screen = self.screen(id);
            if !sampled_stream && label == "stream" && screen.contains("Streaming...") {
                println!("[{label} streaming] {status}\n{}", indent(&screen));
                sampled_stream = true;
            }
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
    project: PathBuf,
    api: Child,
}

impl Fixture {
    fn requests(&self) -> String {
        std::fs::read_to_string(self.temp.path().join("api.log")).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.api.kill();
        let _ = self.api.wait();
    }
}

/// A private HOME and project, with the Engine's inherited environment
/// pointed at them and at the Droid under test, whose only model is the
/// fake API. None when not opted in.
fn fixture() -> Option<Fixture> {
    let Some(bin) = std::env::var_os("DIRI_DROID_BIN_DIR") else {
        eprintln!("DIRI_DROID_BIN_DIR unset; skipping");
        return None;
    };
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    std::fs::create_dir_all(home.join(".factory")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(
        home.join(".factory/settings.json"),
        serde_json::to_vec_pretty(&json!({
            "logoAnimation": "off",
            "customModels": [{
                "model": "fake-model",
                "displayName": "Fake",
                "baseUrl": format!("http://127.0.0.1:{port}"),
                "apiKey": "fake-key-for-diri-e2e",
                "provider": "anthropic",
                "maxOutputTokens": 4096,
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let api = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_droid_api.py"))
        .arg(port.to_string())
        .arg(temp.path().join("api.log"))
        .spawn()
        .expect("python3 for the fake Anthropic API");
    let deadline = Instant::now() + Duration::from_secs(25);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "fake API never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    let path = format!(
        "{}:{}",
        Path::new(&bin).display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let proxy = format!("http://127.0.0.1:{port}");
    // SAFETY: --test-threads=1, and set before the Engine spawns anything.
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("HOME", &home);
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            std::env::set_var(name, &proxy);
        }
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("FACTORY_") {
                std::env::remove_var(name);
            }
        }
        // No Factory account: airgap mode runs only BYOK custom models.
        std::env::set_var("FACTORY_AIRGAP_ENABLED", "true");
        std::env::set_var("FACTORY_DROID_AUTO_UPDATE_ENABLED", "false");
        std::env::set_var("FACTORY_DISABLE_KEYRING", "true");
    }
    Some(Fixture { temp, project, api })
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
        "kind": { "droid": {} },
        "cwd": project,
        "initialCols": 120,
        "initialRows": 40,
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

/// Sends the manifest's Approve action the way the app does; a missing
/// action is a failure.
fn approve(client: &mut Client, id: &str, failures: &mut Vec<String>) {
    let manifest: Value = serde_json::from_str(include_str!("../manifests/droid.json")).unwrap();
    let Some(approve) = manifest["agent"].get("approve") else {
        failures.push("Droid has no manifest Approve action".into());
        return;
    };
    client
        .call(
            "session.send_text",
            json!({ "sessionID": id, "text": approve["text"], "submit": approve["submit"] }),
        )
        .expect("manifest Approve action");
}

/// An initial prompt into a folder Droid has never trusted, a streamed turn,
/// a command that needs approval, an AskUser question, then kill and resume.
#[test]
#[ignore = "needs DIRI_DROID_BIN_DIR and a real Droid"]
fn prompts_stream_ask_permission_and_resume() {
    let Some(fixture) = fixture() else {
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
        status.contains("idle") && screen.contains("⛬  PINEAPPLE")
    });
    settled(&mut failures, "the initial turn", &seen);
    if !fixture
        .requests()
        .contains("tools=True last_user='Reply with the word PINEAPPLE please.'")
    {
        failures.push(format!(
            "the initial prompt never reached the API:\n{}",
            fixture.requests()
        ));
    }

    client.send(&id, "SLOW stream something");
    // Droid shows only "Streaming..." under its spinner and draws the text
    // once the message ends, so the working phase is the spinner itself.
    let seen = client.watch(&id, "stream", Duration::from_secs(20), |status, screen| {
        status.contains("idle") && screen.contains("SLOWDONE")
    });
    if !seen.iter().any(|status| status.contains("working")) {
        failures.push(format!("a streamed turn never read as working: {seen:?}"));
    }
    settled(&mut failures, "the streamed turn", &seen);

    client.send(&id, "RUNCMD for me");
    let seen = client.watch(&id, "permission", Duration::from_secs(20), |status, _| {
        status.contains("needsInput")
    });
    if !seen.iter().any(|status| status.contains("permission")) {
        failures.push(format!(
            "the command approval never read as a permission: {seen:?}"
        ));
    }
    approve(&mut client, &id, &mut failures);
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

    client.send(&id, "ASKME something");
    let seen = client.watch(&id, "question", Duration::from_secs(20), |status, _| {
        status.contains("needsInput")
    });
    if !seen.iter().any(|status| status.contains("question")) {
        failures.push(format!("AskUser never read as a question: {seen:?}"));
    }
    // Enter picks the preselected first option.
    client.send(&id, "");
    let seen = client.watch(
        &id,
        "answered",
        Duration::from_secs(20),
        |status, screen| status.contains("idle") && screen.matches("DONECMD").count() >= 2,
    );
    settled(&mut failures, "the answered question", &seen);

    let _ = client.call("session.kill", json!({ "sessionID": id }));
    client.watch(&id, "killed", Duration::from_secs(5), |status, _| {
        status.contains("exited")
    });
    let list = client.call("session.list", json!({})).unwrap();
    let record = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"] == id.as_str())
        .cloned()
        .unwrap_or_default();
    if record["resumability"] != "resumable" {
        failures.push(format!(
            "an exited Droid is not resumable: {}",
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
    if client.screen(&id).contains("Type to filter sessions") {
        failures.push("resume opened Droid's session picker".into());
    }
    client.send(&id, "Say MANGO now");
    let seen = client.watch(
        &id,
        "after-resume",
        Duration::from_secs(20),
        |status, screen| status.contains("idle") && screen.contains("⛬  MANGO"),
    );
    settled(&mut failures, "the resumed follow-up", &seen);
    let requests = fixture.requests();
    let last = requests
        .lines()
        .rev()
        .find(|line| line.contains("last_user='Say MANGO now'"));
    if !last.is_some_and(|line| line.contains("PINEAPPLE") && line.contains("RUNCMD")) {
        failures.push(format!("resume lost model history:\n{requests}"));
    }
    for prompt in [
        "Reply with the word PINEAPPLE please.",
        "SLOW stream something",
        "RUNCMD for me",
        "ASKME something",
        "Say MANGO now",
    ] {
        if requests
            .lines()
            .filter(|line| {
                line.contains("tools=True") && line.contains(&format!("last_user='{prompt}'"))
            })
            .count()
            != 1
        {
            failures.push(format!(
                "expected exactly one separate request for {prompt}:\n{requests}"
            ));
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
#[ignore = "needs DIRI_DROID_BIN_DIR and a real Droid"]
fn first_run_trust_needs_input_then_idles() {
    let Some(fixture) = fixture() else {
        return;
    };
    let mut client = start(fixture.temp.path());
    let (id, _) = spawn(&mut client, &fixture.project, None);
    client.watch(&id, "trust", Duration::from_secs(30), |status, screen| {
        status.contains("needsInput") && screen.contains("Trust this folder?")
    });
    let trust_status = client.status(&id);
    client.send(&id, "");
    client.watch(&id, "idle", Duration::from_secs(30), |status, screen| {
        status.contains("idle") && screen.contains("? for help")
    });
    let idle_status = client.status(&id);
    let _ = client.call("session.kill", json!({ "sessionID": id }));
    assert!(trust_status.contains("needsInput"), "trust: {trust_status}");
    assert!(idle_status.contains("idle"), "idle: {idle_status}");
}
