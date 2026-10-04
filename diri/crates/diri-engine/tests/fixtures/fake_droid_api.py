#!/usr/bin/env python3
"""Scripted stand-in for the Anthropic Messages API, keyed on the latest user text.

  SLOW    -> streams a reply over ~5s (exercises the Working state)
  RUNCMD  -> asks to run a shell command (exercises the permission prompt)
  ASKME   -> asks the user a question (exercises Droid's AskUser dialog)
  other   -> replies with the first ALLCAPS word of the prompt, or OK
A request without tools (title generation) gets a short title. Every reply
waits ~1s first, like a real API. Requests are logged to argv[2].

The server is also the client's HTTP(S) proxy: it logs and refuses every
request for another host, so the test sees, and blocks, any network the CLI
attempts beyond the model API. Used by tests/droid_real.rs; binds
127.0.0.1 only.
"""
import json, re, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = open(sys.argv[2], "a", buffering=1)


def text_of(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(p.get("text", "") for p in content if isinstance(p, dict) and p.get("type") == "text")
    return ""


def last_turn(messages):
    """The text of the latest user message, or a marker when a tool answered last."""
    for message in reversed(messages):
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if isinstance(content, list) and any(
            isinstance(p, dict) and p.get("type") == "tool_result" for p in content
        ):
            return "__TOOL_RESULT__"
        # Droid sends its context as <system-reminder> blocks beside the
        # typed text; the typed text is the last plain block.
        blocks = content if isinstance(content, list) else [{"type": "text", "text": content or ""}]
        typed = [
            p.get("text", "").strip()
            for p in blocks
            if isinstance(p, dict) and p.get("type") == "text"
            and p.get("text", "").strip()
            and not p.get("text", "").strip().startswith("<system-reminder>")
        ]
        if typed:
            return typed[-1]
    return ""


def users(messages):
    return [last_turn([m]) for m in messages if m.get("role") == "user"]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def refuse(self):
        LOG.write(f"PROXY {self.command} {self.path}\n")
        self.send_response(403)
        self.send_header("content-length", "0")
        self.send_header("connection", "close")
        self.end_headers()
        self.close_connection = True

    def do_CONNECT(self):
        self.refuse()

    def proxied(self):
        return self.path.startswith("http://") or self.path.startswith("https://")

    def do_GET(self):
        if self.proxied():
            return self.refuse()
        LOG.write(f"GET {self.path}\n")
        self.reply_json({"data": []})

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(length) or b"{}"
        if self.proxied():
            return self.refuse()
        body = json.loads(raw)
        messages = body.get("messages", [])
        text = last_turn(messages)
        tools = {tool.get("name") for tool in body.get("tools", [])}
        LOG.write(f"POST {self.path} tools={bool(tools)} last_user={text!r} history={users(messages)!r}\n")

        blocks = []
        if not tools:
            blocks = [("text", ["Fake title"])]
        elif text == "__TOOL_RESULT__":
            blocks = [("text", ["The tool ran. DONECMD"])]
        elif "RUNCMD" in text and "Execute" in tools:
            blocks = [("text", ["Creating the file."]), ("tool_use", ("Execute", {
                "summary": "Create the marker file",
                "command": "touch diri-e2e-file",
                "riskLevel": "medium",
                "riskLevelReason": "creates a file",
            }))]
        elif "ASKME" in text and "AskUser" in tools:
            blocks = [("tool_use", ("AskUser", {
                "questionnaire": "1. [question] Which color should the button use?\n"
                "[topic] Color\n[option] Red\n[option] Blue",
            }))]
        elif "SLOW" in text:
            blocks = [("text", [f"slow part {i} " for i in range(10)] + ["SLOWDONE"])]
        else:
            word = next(iter(re.findall(r"\b[A-Z]{4,}\b", text)), "OK")
            blocks = [("text", [word])]
        stop = "tool_use" if any(kind == "tool_use" for kind, _ in blocks) else "end_turn"

        time.sleep(1.0)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("transfer-encoding", "chunked")
        self.end_headers()
        self.event("message_start", {"type": "message_start", "message": {
            "id": "msg_diri", "type": "message", "role": "assistant",
            "model": body.get("model", "fake-model"), "content": [],
            "stop_reason": None, "usage": {"input_tokens": 10, "output_tokens": 1},
        }})
        for index, (kind, value) in enumerate(blocks):
            if kind == "text":
                self.event("content_block_start", {"type": "content_block_start", "index": index,
                                                   "content_block": {"type": "text", "text": ""}})
                for piece in value:
                    self.event("content_block_delta", {"type": "content_block_delta", "index": index,
                                                       "delta": {"type": "text_delta", "text": piece}})
                    if "SLOW" in text and tools:
                        time.sleep(0.5)
            else:
                name, arguments = value
                self.event("content_block_start", {"type": "content_block_start", "index": index,
                                                   "content_block": {"type": "tool_use", "id": f"toolu_diri_{index}",
                                                                     "name": name, "input": {}}})
                self.event("content_block_delta", {"type": "content_block_delta", "index": index,
                                                   "delta": {"type": "input_json_delta",
                                                             "partial_json": json.dumps(arguments)}})
            self.event("content_block_stop", {"type": "content_block_stop", "index": index})
        self.event("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop},
                                     "usage": {"output_tokens": 5}})
        self.event("message_stop", {"type": "message_stop"})
        self.write_chunk("")

    def event(self, kind, value):
        self.write_chunk(f"event: {kind}\ndata: {json.dumps(value)}\n\n")

    def write_chunk(self, data):
        raw = data.encode()
        self.wfile.write(f"{len(raw):x}\r\n".encode() + raw + b"\r\n")
        self.wfile.flush()

    def reply_json(self, value):
        raw = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
