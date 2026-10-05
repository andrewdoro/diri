#!/usr/bin/env python3
"""Scripted stand-in for the Gemini API as the Antigravity CLI (agy) calls it.

agy talks to the Gemini API directly with `modelProvider: "gemini"` in its
settings.json, GEMINI_API_KEY and GOOGLE_GEMINI_BASE_URL. Keyed on the latest
user text:

  SLOW    -> streams a reply over ~5s (exercises the Working state)
  RUNCMD  -> proposes `run_command` (exercises the permission prompt)
  WRITEFILE -> proposes `write_to_file` (the file-edit permission)
  ASKQ    -> calls `ask_question` (exercises the question modal)
  other   -> replies with the first ALLCAPS word of the prompt, or OK
Every reply waits ~1s first, like the real API. Requests are logged to argv[2]; argv[3] is the workspace.
Used by tests/antigravity_real.rs; binds 127.0.0.1 only.
"""
import json, re, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = open(sys.argv[2], "a", buffering=1)
# run_command must name a Cwd inside the workspace.
CWD = sys.argv[3]


def answered_tool(body):
    """agy sends a tool's result as the newest content (under role "model")."""
    contents = body.get("contents", [])
    return bool(contents) and any("functionResponse" in p for p in contents[-1].get("parts", []))


def newest_user_request(body):
    """agy wraps each prompt in <USER_REQUEST>; find the newest one."""
    for content in reversed(body.get("contents", [])):
        for part in content.get("parts", []):
            match = re.search(r"<USER_REQUEST>\s*(.*?)\s*</USER_REQUEST>", part.get("text", ""), re.S)
            if match:
                return match.group(1)
    return ""


def candidate(parts):
    return {
        "candidates": [{"content": {"role": "model", "parts": parts}, "finishReason": "STOP", "index": 0}],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15},
        "modelVersion": "gemini-3.1-pro-preview",
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        LOG.write(f"GET {self.path}\n")
        self.reply_json({"models": []})

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        request = newest_user_request(body)
        LOG.write(f"POST {self.path} last_user={request[:80]!r}\n")

        if ":countTokens" in self.path:
            return self.reply_json({"totalTokens": 10})
        # The flash-lite side calls (titles, summaries) only need some text.
        if "flash-lite" in self.path:
            return self.stream([[{"text": "Fake title"}]], slow=False)

        if answered_tool(body):
            chunks = [[{"text": "The tool ran. DONECMD"}]]
        elif "RUNCMD" in request:
            chunks = [[{"functionCall": {"name": "run_command", "args": {
                "CommandLine": "touch diri-e2e-file",
                "Cwd": CWD,
                "WaitMsBeforeAsync": 5000,
                "toolAction": "Creating file",
                "toolSummary": "File creation",
            }}}]]
        elif "WRITEFILE" in request:
            chunks = [[{"functionCall": {"name": "write_to_file", "args": {
                "TargetFile": CWD + "/diri-e2e-notes.txt",
                "CodeContent": "written by the fake model\n",
                "Description": "Create a notes file",
                "Overwrite": False,
                "toolAction": "Writing file",
                "toolSummary": "File write",
            }}}]]
        elif "ASKQ" in request:
            chunks = [[{"functionCall": {"name": "ask_question", "args": {
                "questions": [{"question": "Which flavour do you want?", "options": ["Vanilla", "Chocolate"]}],
                "toolAction": "Asking question",
                "toolSummary": "Flavour question",
            }}}]]
        elif "SLOW" in request:
            chunks = [[{"text": f"slow part {i} "}] for i in range(10)] + [[{"text": "SLOWDONE"}]]
        else:
            word = next(iter(re.findall(r"\b[A-Z]{4,}\b", request)), "OK")
            chunks = [[{"text": word}]]

        time.sleep(1.0)
        self.stream(chunks, slow="SLOW" in request)

    def stream(self, chunks, slow):
        if ":streamGenerateContent" not in self.path:
            return self.reply_json(candidate([p for parts in chunks for p in parts]))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("transfer-encoding", "chunked")
        self.end_headers()
        for parts in chunks:
            self.chunk(f"data: {json.dumps(candidate(parts))}\r\n\r\n")
            if slow:
                time.sleep(0.5)
        self.chunk("")

    def chunk(self, data):
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
