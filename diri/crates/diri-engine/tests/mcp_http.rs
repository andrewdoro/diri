//! The Engine-served `dirijor` MCP endpoint, driven end to end over HTTP the
//! way Claude Code and Codex drive it: initialize, tools/list, a tools/call
//! that resolves the calling session from its bearer token, cancellation of
//! a long wait from a second request, and token death with the session.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use diri_engine::control::{ControlServer, InjectionConfig};
use diri_engine::{ManifestEngine, Registry};
use serde_json::{Value, json};

struct Engine {
    server: Arc<ControlServer>,
    registry: Arc<Mutex<Registry>>,
    url: String,
    _temp: tempfile::TempDir,
}

fn start() -> Engine {
    let temp = tempfile::tempdir().unwrap();
    let (manifests, _) =
        ManifestEngine::load_dir(&diri_engine::detect::bundled_manifest_dir()).unwrap();
    let registry = Arc::new(Mutex::new(Registry::new(
        Arc::new(manifests),
        temp.path().join("state.json"),
    )));
    let server = Arc::new(
        ControlServer::new(Arc::clone(&registry), temp.path().join("daemon.sock"))
            .with_config_dir(temp.path().join("config"))
            .with_notes_dir(temp.path().join("notes"))
            .with_injection(InjectionConfig {
                inject_dir: temp.path().join("inject"),
                cli_path: temp.path().join("bin/dirijor"),
            }),
    );
    let listener = server.bind().unwrap();
    {
        let server = Arc::clone(&server);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let server = Arc::clone(&server);
                std::thread::spawn(move || {
                    let _ = server.serve(stream);
                });
            }
        });
    }
    let url = server.start_mcp_http().unwrap();
    Engine {
        server,
        registry,
        url,
        _temp: temp,
    }
}

fn spawn_session(engine: &Engine, cwd: &Path) -> String {
    let record = dirijor_mcp::Bridge::new(engine.server.socket_path().to_owned(), None)
        .request(
            "session.spawn",
            json!({"kind":{"shell":{}}, "cwd":cwd, "argv":["/bin/sh","-c","sleep 60"]}),
            Duration::from_secs(20),
        )
        .unwrap();
    record["id"].as_str().unwrap().to_owned()
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    /// The JSON-RPC message in a JSON body or in the last SSE `data:` line.
    fn message(&self) -> Value {
        if self.head.contains("text/event-stream") {
            let data = self
                .body
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .next_back()
                .unwrap_or_else(|| panic!("no event in {:?}", self.body));
            serde_json::from_str(data).unwrap()
        } else {
            serde_json::from_str(&self.body).unwrap()
        }
    }
}

fn post(url: &str, token: Option<&str>, body: &Value) -> Reply {
    let address = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let body = serde_json::to_string(body).unwrap();
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    Reply {
        status: head[9..12].parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

fn call(url: &str, token: &str, id: u64, tool: &str, arguments: Value) -> Value {
    let reply = post(
        url,
        Some(token),
        &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
    );
    assert_eq!(reply.status, 200, "{}", reply.head);
    reply.message()
}

#[test]
fn an_agent_reaches_its_own_session_over_http() {
    let engine = start();
    let cwd = tempfile::tempdir().unwrap();
    let session = spawn_session(&engine, cwd.path());
    let token = engine.server.mcp_http_token(&session).unwrap();
    assert!(!token.is_empty());

    // The shared Claude config names the variable and never holds a token.
    let config =
        std::fs::read_to_string(engine._temp.path().join("inject/claude-mcp-http.json")).unwrap();
    let config: Value = serde_json::from_str(&config).unwrap();
    assert_eq!(config["mcpServers"]["dirijor"]["type"], "http");
    assert_eq!(config["mcpServers"]["dirijor"]["url"], engine.url);
    assert_eq!(
        config["mcpServers"]["dirijor"]["headers"]["Authorization"],
        "Bearer ${DIRIJOR_MCP_TOKEN}"
    );

    let initialized = post(
        &engine.url,
        Some(&token),
        &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"test","version":"1"}}}),
    );
    assert_eq!(initialized.status, 200);
    let initialized = initialized.message();
    assert_eq!(initialized["result"]["serverInfo"]["name"], "dirijor");
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("INSIDE Diri")
    );
    let ack = post(
        &engine.url,
        Some(&token),
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    assert_eq!(ack.status, 202);

    let listed = post(
        &engine.url,
        Some(&token),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .message();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for expected in [
        "whoami",
        "spawn_agent",
        "wait_for_agent",
        "wait_any",
        "browser",
        "read_note",
        "schedule_agent",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    // Identical to what the stdio frontend lists for the same Engine.
    let stdio = dirijor_mcp::Bridge::new(engine.server.socket_path().to_owned(), None)
        .tool_definitions()
        .unwrap()
        .iter()
        .map(|tool| tool.wire_value())
        .collect::<Vec<_>>();
    assert_eq!(listed["result"]["tools"], Value::Array(stdio));

    let whoami = call(&engine.url, &token, 2, "whoami", json!({}));
    assert_eq!(whoami["result"]["isError"], false, "{whoami}");
    let identity = &whoami["result"]["structuredContent"];
    assert_eq!(identity["hosted"], true);
    assert_eq!(identity["session"]["id"], session, "{identity}");

    // A mutation runs under the caller's identity: the write policy sees
    // this session and refuses to let it manage itself, as over stdio.
    let own = call(
        &engine.url,
        &token,
        3,
        "manage_agent",
        json!({"session_id": session, "action": "hibernate"}),
    );
    assert_eq!(own["result"]["isError"], true, "{own}");
    assert_eq!(
        own["result"]["content"][0]["text"],
        "an agent cannot manage its own session"
    );

    // Other tokens: unknown, forged for another id, or no token at all.
    let ping = json!({"jsonrpc":"2.0","id":9,"method":"ping"});
    assert_eq!(post(&engine.url, None, &ping).status, 401);
    assert_eq!(post(&engine.url, Some("nope"), &ping).status, 401);
    let (_, mac) = token.rsplit_once('.').unwrap();
    assert_eq!(
        post(&engine.url, Some(&format!("s-other.{mac}")), &ping).status,
        401
    );
    assert_eq!(post(&engine.url, Some(&token), &ping).status, 200);

    // A long read is cancelled by a notification on another request.
    let waiter = {
        let url = engine.url.clone();
        let token = token.clone();
        let session = session.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let reply = post(
                &url,
                Some(&token),
                &json!({"jsonrpc":"2.0","id":"long","method":"tools/call","params":{
                    "name":"wait_for_agent",
                    "arguments":{"session_id":session,"until":"exited","timeout_s":600}}}),
            );
            (reply, started.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(500));
    let cancelled = post(
        &engine.url,
        Some(&token),
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"long"}}),
    );
    assert_eq!(cancelled.status, 202);
    let (reply, elapsed) = waiter.join().unwrap();
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    assert_eq!(reply.status, 200);
    assert!(
        !reply.body.contains("\"result\""),
        "a cancelled request gets no response: {}",
        reply.body
    );

    // The token dies with the session.
    engine
        .registry
        .lock()
        .unwrap()
        .terminate(&session, Duration::from_secs(2))
        .unwrap();
    assert_eq!(post(&engine.url, Some(&token), &ping).status, 401);
}
