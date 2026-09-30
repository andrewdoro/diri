//! Scheduled tasks, end to end: a real Engine on a private socket starts a
//! real shell session when a schedule comes due, and on startup catches up
//! an occurrence it was not running for. The prompt is a shell command that
//! touches a marker file, so "the run happened" is observed from outside.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use diri_engine::control::ControlServer;
use diri_engine::detect::ManifestEngine;
use diri_engine::registry::Registry;
use diri_engine::session::HolderConfig;
use diri_proto::ControlMessage;
use serde_json::{Value, json};

fn engine() -> Arc<ManifestEngine> {
    let dir = diri_engine::detect::bundled_manifest_dir()
        .canonicalize()
        .expect("manifests");
    let (engine, _) = ManifestEngine::load_dir(&dir).expect("load");
    Arc::new(engine)
}

/// The Engine as `dirijord-rs` runs it, minus the scheduler, which each test
/// starts itself so it can stage the journal first.
fn start_server(temp: &Path) -> Arc<ControlServer> {
    let registry = Arc::new(Mutex::new(Registry::new(engine(), temp.join("state.json"))));
    let server = Arc::new(
        ControlServer::new(Arc::clone(&registry), temp.join("daemon.sock"))
            .with_logs_dir(temp.join("logs"))
            .with_holder(HolderConfig {
                holders_dir: temp.join("holders"),
                executable: env!("CARGO_BIN_EXE_diri-holder").into(),
            }),
    );
    let listener = server.bind().expect("bind");
    {
        let server = Arc::clone(&server);
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    let _ = server.serve(stream);
                });
            }
        });
    }
    server
}

struct Control {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Control {
    fn connect(server: &ControlServer) -> Self {
        let stream = UnixStream::connect(server.socket_path()).expect("connect control");
        let reader = BufReader::new(stream.try_clone().expect("clone"));
        Self {
            stream,
            reader,
            next_id: 1,
        }
    }

    fn try_request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut bytes = serde_json::to_vec(&ControlMessage::Request {
            id,
            method: method.into(),
            params: Some(params),
        })
        .expect("encode");
        bytes.push(b'\n');
        self.stream.write_all(&bytes).expect("write");
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read reply");
        match serde_json::from_str::<ControlMessage>(&line).expect("decode") {
            ControlMessage::Response { result, .. } => result.map_err(|error| error.message),
            other => panic!("{method}: unexpected {other:?}"),
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.try_request(method, params)
            .unwrap_or_else(|error| panic!("{method} failed: {error}"))
    }

    fn schedule(&mut self, id: &str) -> Value {
        self.request("schedule.list", json!({}))["schedules"]
            .as_array()
            .expect("schedules")
            .iter()
            .find(|record| record["id"] == id)
            .cloned()
            .expect("schedule listed")
    }
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn spec(marker: &Path, when: Value, window_ms: u64) -> Value {
    json!({
        "title": "Touch the marker",
        "when": when,
        "spawn": {
            "kind": { "shell": {} },
            "cwd": "/tmp",
            "initialPrompt": format!("touch '{}'", marker.display()),
        },
        "catchUpWindowMs": window_ms,
    })
}

/// Moves a stored schedule's next due time, as if the Engine had been down
/// when it came due.
fn backdate(temp: &Path, id: &str, due_ms: f64) {
    let db = rusqlite::Connection::open(temp.join("schedules-v1.sqlite")).expect("open");
    let raw: String = db
        .query_row("SELECT record FROM schedules_v1 WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .expect("row");
    let mut record: Value = serde_json::from_str(&raw).unwrap();
    record["nextDue"] = json!(due_ms);
    db.execute(
        "UPDATE schedules_v1 SET record=?1 WHERE id=?2",
        rusqlite::params![record.to_string(), id],
    )
    .expect("update");
}

fn kill_run(control: &mut Control, record: &Value) {
    if let Some(session) = record["runs"][0]["sessionId"].as_str() {
        control.request("session.kill", json!({ "sessionID": session }));
    }
}

#[test]
fn a_due_one_shot_starts_its_session_on_time() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_server(temp.path());
    server.spawn_scheduler();
    let mut control = Control::connect(&server);
    let marker = temp.path().join("fired");

    let created = control.request(
        "schedule.create",
        spec(
            &marker,
            json!({ "kind": "once", "at": now_ms() + 1_500.0 }),
            60_000,
        ),
    );
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["enabled"], true);

    wait_until("the scheduled prompt ran", Duration::from_secs(30), || {
        marker.exists()
    });
    let mut record = control.schedule(&id);
    wait_until("the run recorded its session", Duration::from_secs(10), || {
        record = control.schedule(&id);
        record["runs"][0]["sessionId"].is_string()
    });
    assert_eq!(record["runs"][0]["outcome"], "onTime");
    assert_eq!(record["enabled"], false, "a one-shot ends after its run");
    assert!(record.get("nextDue").is_none());
    kill_run(&mut control, &record);
}

#[test]
fn an_occurrence_missed_while_not_running_fires_once_late() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_server(temp.path());
    let mut control = Control::connect(&server);
    let marker = temp.path().join("caught-up");

    let created = control.request(
        "schedule.create",
        spec(
            &marker,
            json!({ "kind": "cron", "expr": "0 9 * * *" }),
            12 * 3_600_000,
        ),
    );
    let id = created["id"].as_str().unwrap().to_owned();
    // Due ten minutes ago, before this Engine started: the 9:00-but-it's-9:10 case.
    backdate(temp.path(), &id, now_ms() - 10.0 * 60_000.0);
    server.spawn_scheduler();

    wait_until("the missed run caught up", Duration::from_secs(30), || {
        marker.exists()
    });
    let mut record = control.schedule(&id);
    wait_until("the run recorded its session", Duration::from_secs(10), || {
        record = control.schedule(&id);
        record["runs"][0]["sessionId"].is_string()
    });
    let runs = record["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "caught up exactly once: {runs:?}");
    assert_eq!(runs[0]["outcome"], "late");
    assert_eq!(runs[0]["lateReason"], "notRunning");
    assert_eq!(record["enabled"], true);
    assert!(record["nextDue"].as_f64().unwrap() > now_ms(), "re-armed ahead");
    kill_run(&mut control, &record);
}

#[test]
fn an_occurrence_past_its_window_is_recorded_missed_and_not_run() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_server(temp.path());
    let mut control = Control::connect(&server);
    let marker = temp.path().join("should-not-exist");

    let created = control.request(
        "schedule.create",
        spec(
            &marker,
            json!({ "kind": "cron", "expr": "0 9 * * *" }),
            60_000,
        ),
    );
    let id = created["id"].as_str().unwrap().to_owned();
    backdate(temp.path(), &id, now_ms() - 10.0 * 60_000.0);
    server.spawn_scheduler();

    let mut record = control.schedule(&id);
    wait_until("the miss was recorded", Duration::from_secs(10), || {
        record = control.schedule(&id);
        record["runs"].as_array().is_some_and(|runs| !runs.is_empty())
    });
    assert_eq!(record["runs"][0]["outcome"], "missed");
    assert!(record["runs"][0].get("sessionId").is_none());
    std::thread::sleep(Duration::from_secs(2));
    assert!(!marker.exists(), "a missed run must not start anything");
    assert!(record["nextDue"].as_f64().unwrap() > now_ms());
}

#[test]
fn run_now_starts_a_run_and_keeps_the_next_due_time() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_server(temp.path());
    server.spawn_scheduler();
    let mut control = Control::connect(&server);
    let marker = temp.path().join("manual");

    let created = control.request(
        "schedule.create",
        spec(
            &marker,
            json!({ "kind": "cron", "expr": "0 9 * * *" }),
            60_000,
        ),
    );
    let id = created["id"].as_str().unwrap().to_owned();
    control.request("schedule.run_now", json!({ "id": id }));
    wait_until("the manual run ran", Duration::from_secs(30), || {
        marker.exists()
    });
    let mut record = control.schedule(&id);
    wait_until("the run recorded its session", Duration::from_secs(10), || {
        record = control.schedule(&id);
        record["runs"][0]["sessionId"].is_string()
    });
    assert_eq!(record["runs"][0]["outcome"], "manual");
    assert_eq!(record["nextDue"], created["nextDue"]);
    kill_run(&mut control, &record);

    control.request("schedule.delete", json!({ "id": id }));
    assert!(
        control.request("schedule.list", json!({}))["schedules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(control.try_request("schedule.delete", json!({ "id": id })).is_err());
}

#[test]
fn invalid_schedules_are_rejected_before_storage() {
    let temp = tempfile::tempdir().unwrap();
    let server = start_server(temp.path());
    let mut control = Control::connect(&server);
    let marker = temp.path().join("never");
    let error = control
        .try_request(
            "schedule.create",
            spec(&marker, json!({ "kind": "cron", "expr": "at nine" }), 0),
        )
        .unwrap_err();
    assert!(error.contains("five fields"), "{error}");
    let error = control
        .try_request(
            "schedule.create",
            spec(&marker, json!({ "kind": "once", "at": now_ms() - 3_600_000.0 }), 0),
        )
        .unwrap_err();
    assert!(error.contains("past"), "{error}");
    assert!(
        control.request("schedule.list", json!({}))["schedules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
