//! A Claude spawn with an initial prompt must not sit out the whole
//! workspace-trust watch when the folder is already trusted. Claude runs no
//! hooks before trust, so its first hook ends the watch; before that exit
//! every such spawn RPC took 20s longer than the delivery itself.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use diri_engine::control::ControlServer;
use diri_engine::detect::ManifestEngine;
use diri_engine::registry::Registry;
use diri_proto::{ControlError, ControlMessage};
use serde_json::json;

struct Control {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Control {
    fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("connect");
        let reader = BufReader::new(stream.try_clone().expect("clone"));
        Self {
            stream,
            reader,
            next_id: 1,
        }
    }

    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ControlError> {
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
            ControlMessage::Response { result, .. } => result,
            other => panic!("{method} failed: {other:?}"),
        }
    }
}

/// The bundled Claude manifest, minus its binary so the fixture argv runs.
fn claude_engine(temp: &Path) -> Arc<ManifestEngine> {
    let manifests = temp.join("manifests");
    std::fs::create_dir(&manifests).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(diri_engine::detect::bundled_manifest_dir().join("claude-code.json"))
            .unwrap(),
    )
    .unwrap();
    manifest["agent"].as_object_mut().unwrap().remove("binary");
    std::fs::write(
        manifests.join("claude-code.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let (engine, _) = ManifestEngine::load_dir(&manifests).unwrap();
    Arc::new(engine)
}

fn start_server(temp: &Path) -> Arc<ControlServer> {
    let registry = Arc::new(Mutex::new(Registry::new(
        claude_engine(temp),
        temp.join("state.json"),
    )));
    let server = Arc::new(
        ControlServer::new(Arc::clone(&registry), temp.join("daemon.sock"))
            .with_logs_dir(temp.join("logs")),
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

#[test]
fn a_hook_ends_the_trust_watch_so_a_trusted_spawn_returns_promptly() {
    let temp = tempfile::tempdir().expect("temp");
    let server = start_server(temp.path());
    let socket = server.socket_path().to_path_buf();

    // Stand-in for Claude in a trusted folder: no trust picker, just a
    // composer, and a SessionStart hook once it is up.
    let hooks = {
        let socket = socket.clone();
        std::thread::spawn(move || {
            let mut control = Control::connect(&socket);
            let deadline = Instant::now() + Duration::from_secs(10);
            let id = loop {
                let listed = control.request("session.list", json!({})).expect("list");
                if let Some(id) = listed["sessions"][0]["id"].as_str() {
                    break id.to_owned();
                }
                assert!(Instant::now() < deadline, "the session never appeared");
                std::thread::sleep(Duration::from_millis(50));
            };
            std::thread::sleep(Duration::from_millis(500));
            control
                .request(
                    "hook.report",
                    json!({
                        "kind": "claude-hook", "dirijorSessionID": id,
                        "event": "SessionStart",
                        "payload": {"hook_event_name": "SessionStart", "session_id": "c-1"},
                    }),
                )
                .expect("hook");
            id
        })
    };

    let mut control = Control::connect(&socket);
    let started = Instant::now();
    let result = control.request(
        "session.spawn",
        json!({
            "kind": { "claude-code": {} }, "cwd": "/tmp",
            "argv": ["/bin/sh", "-c", r#"stty -echo; printf '\033[?2004h> '; exec cat"#],
            "initialPrompt": "acknowledge this prompt",
        }),
    );
    let elapsed = started.elapsed();
    let id = hooks.join().expect("hook thread");
    let _ = control.request("session.kill", json!({ "sessionID": id }));

    assert!(result.is_ok(), "prompt delivery failed: {result:?}");
    assert!(
        elapsed < Duration::from_secs(12),
        "spawn waited out the trust watch after the hook: {elapsed:?}"
    );
}
