mod runtime;

use dirijor_mcp::Bridge;
use serde_json::{Value, json};

trait ToolBackend {
    fn tools(&mut self) -> Result<Value, String>;
    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String>;
}

struct DirectBackend {
    bridge: Bridge,
}

impl DirectBackend {
    fn new() -> Self {
        Self {
            bridge: Bridge::default(),
        }
    }
}

impl ToolBackend for DirectBackend {
    fn tools(&mut self) -> Result<Value, String> {
        let tools = json!({
            "tools": self
                .bridge
                .tool_definitions()?
                .iter()
                .map(|tool| tool.wire_value())
                .collect::<Vec<_>>()
        });
        Ok(tools)
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String> {
        self.bridge.call(name, arguments)
    }
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.into()}})
}

fn tool_content(result: Result<Value, String>) -> Value {
    let (value, is_error) = match result {
        Ok(value) => {
            let is_error = value.get("ok") == Some(&Value::Bool(false));
            (value, is_error)
        }
        Err(message) => (Value::String(message), true),
    };
    let text = value.as_str().map_or_else(
        || serde_json::to_string(&value).unwrap_or_else(|_| "null".to_owned()),
        str::to_owned,
    );
    // Typed results for clients that read structuredContent (MCP 2025-06-18);
    // the text block stays for every other client.
    if value.is_object() && !is_error {
        return json!({"content":[{"type":"text","text":text}],"structuredContent":value,"isError":false});
    }
    json!({"content":[{"type":"text","text":text}],"isError":is_error})
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| matches!(*version, "2024-11-05" | "2025-03-26" | "2025-06-18"))
        .unwrap_or("2025-06-18");
    let browser = if std::env::var_os("DIRIJOR_TEST_RUN_AVAILABLE").is_some() {
        " To test a web feature, use test_run with a preview URL from get_artifacts."
    } else {
        ""
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools":{}},
        "serverInfo": {"name":"dirijor","version":"0.1.0"},
        "instructions": format!(
            "This session is running INSIDE Diri, a desktop orchestrator for coding agents. \
             These tools control it. Use them proactively whenever the user asks to \
             open/start/spawn/close another agent, session, tab, or terminal (Claude Code, \
             Codex, Cursor, Gemini, or a shell), to check what other sessions are doing, to \
             talk to another session, or to parallelize work across git worktrees — no \
             extra confirmation of intent needed.\n\n\
             Parallel work (preferred): spawn_agents with one entry per subtask \
             (worktree:true, prompt, task:true) → wait_any(task_ids) → for each ready task \
             read its result (read_output mode:last_message for detail), answer_task if it is \
             blocked, get_diff to review, integrate to bring its branch into your checkout → \
             call wait_any again with the pending ids → release_agent when done. wait_any \
             returns as soon as ANY target needs you; do not wait for all of them at once.\n\n\
             Agent vs terminal rule: to spawn another agent, select its native kind (for \
             example `claude` or `codex`) and pass its task as `prompt`. If no agent is named, \
             use your own native kind when available. Never use `shell` to launch an agent CLI \
             such as `claude`, `codex`, `cursor`, or `gemini`; a child `shell` is a raw \
             terminal in the parent's Cmd+J pane whose prompt runs as shell commands.\n\n\
             When you receive a Diri task: report_task acknowledged before starting, then \
             completed or failed for that exact task_id after verifying (JSON matching \
             result_schema when the task has one), or blocked with your question. \
             report_to_parent is recorded on your open task automatically.\n\n\
             Delivery rules: messages, spawns, and tasks are deduplicated and delivered at \
             most once. Reuse message_id/operation_id/request_id on retries; never resend \
             under a new identity because an agent is slow or its screen is unchanged. A \
             delivery receipt does not mean the work is done. For untracked prompts, pass the \
             returned since_ms to wait_for_agent/wait_any so an agent that was already idle \
             does not count as finished.\n\n\
             Also: get_artifacts returns PR/preview URLs and ports (PRs include live GitHub \
             status); fork_agent branches a conversation to try an alternative; manage_agent \
             hibernates idle children instead of killing them; quick_open_include edits the \
             folders Cmd+P indexes (e.g. `**/.worktrees/`).{browser}\n\n\
             Notes: {notes}",
            notes = dirijor_mcp::tools::NOTES_CONTRACT,
        )
    })
}

fn handle_message(message: Value, backend: &mut impl ToolBackend) -> Option<Value> {
    let object = match message.as_object() {
        Some(object) => object,
        None => return Some(error(Value::Null, -32600, "Invalid Request")),
    };
    let id = object.get("id").cloned();
    let response_id = id
        .clone()
        .filter(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some())
        .unwrap_or(Value::Null);
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(error(
            response_id,
            -32600,
            "Invalid Request: method must be a string",
        ));
    };
    if object.get("jsonrpc") != Some(&json!("2.0"))
        || id
            .as_ref()
            .is_some_and(|id| !(id.is_string() || id.as_i64().is_some() || id.as_u64().is_some()))
    {
        return Some(error(response_id, -32600, "Invalid Request"));
    }
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return id.map(|id| error(id, -32602, "params must be an object"));
    }

    match method {
        "initialize" => id.map(|id| success(id, initialize(&params))),
        "ping" => id.map(|id| success(id, json!({}))),
        "tools/list" => id.map(|id| match backend.tools() {
            Ok(tools) => success(id, tools),
            Err(message) => error(id, -32603, message),
        }),
        "tools/call" => id.map(|id| {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return success(
                    id,
                    tool_content(Err("tools/call missing 'name'".to_owned())),
                );
            };
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            success(id, tool_content(backend.call(name, &arguments)))
        }),
        _ if id.is_none() => None,
        _ => Some(error(
            id.unwrap_or(Value::Null),
            -32601,
            format!("Method not found: {method}"),
        )),
    }
}

fn main() {
    runtime::serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl ToolBackend for Fake {
        fn tools(&mut self) -> Result<Value, String> {
            Ok(json!({"tools":[{"name":"list_agents"}]}))
        }

        fn call(&mut self, name: &str, _: &Value) -> Result<Value, String> {
            (name == "list_agents")
                .then(|| json!({"agents":[]}))
                .ok_or_else(|| "unknown tool".to_owned())
        }
    }

    #[test]
    fn unknown_delivery_is_not_marked_as_a_successful_tool_call() {
        let result = tool_content(Ok(json!({"ok":false, "receipt":{"delivery":"unknown"}})));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn serves_mcp_through_a_rust_backend() {
        let mut backend = Fake;
        let listed = handle_message(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            &mut backend,
        )
        .unwrap();
        assert_eq!(listed["result"]["tools"][0]["name"], "list_agents");

        let called = handle_message(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_agents","arguments":{}}}),
            &mut backend,
        )
        .unwrap();
        assert_eq!(called["result"]["isError"], false);
        assert_eq!(called["result"]["content"][0]["text"], "{\"agents\":[]}");
    }

    #[test]
    fn instructions_distinguish_agent_sessions_from_shell_panes() {
        let initialized = initialize(&json!({}));
        let instructions = initialized["instructions"].as_str().expect("instructions");

        assert!(instructions.contains("native kind"));
        assert!(instructions.contains("Never use `shell` to launch an agent CLI"));
        assert!(instructions.contains("Cmd+J"));
    }

    #[test]
    fn instructions_teach_the_notes_contract() {
        let initialized = initialize(&json!({}));
        let instructions = initialized["instructions"].as_str().expect("instructions");
        for step in [
            "read it first with read_note {\"note\":\"origin\"}",
            "a decision, a finding, a blocker, a result, a link",
            "no progress chatter",
            "Never rewrite or delete the person's text",
            "Tick your own sub-tasks",
            "the person reviews your work and ticks it",
            "Finish with a one-paragraph result",
            "create_note",
            "open:true",
        ] {
            assert!(instructions.contains(step), "missing: {step}");
        }
    }
}
