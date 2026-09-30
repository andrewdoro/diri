#!/usr/bin/env python3
"""Scripted stand-in for the Gemini API, keyed on the latest user text.

  SLOW    -> streams a reply over ~5s (exercises the Working state)
  RUNCMD  -> asks to run a shell command (exercises the permission dialog)
  other   -> replies with the first ALLCAPS word of the prompt, or OK
Every reply waits ~1s first, like the real API. Requests are logged to argv[2].
Used by tests/gemini_real.rs; binds 127.0.0.1 only.
"""
import json, re, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = open(sys.argv[2], "a", buffering=1)


def last_user_text(body):
    for content in reversed(body.get("contents", [])):
        if content.get("role") != "user":
            continue
        parts = content.get("parts", [])
        if any("functionResponse" in p for p in parts):
            return "__FUNCTION_RESPONSE__"
        texts = [p["text"] for p in parts if p.get("text", "").strip()]
        if texts:
            return texts[-1]
    return ""


def candidate(parts):
    return {
        "candidates": [{"content": {"role": "model", "parts": parts}, "finishReason": "STOP", "index": 0}],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15},
        "modelVersion": "gemini-2.5-flash",
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
        text = last_user_text(body)
        config = body.get("generationConfig", {})
        structured = "responseJsonSchema" in config or "responseSchema" in config or config.get("responseMimeType") == "application/json"
        LOG.write(f"POST {self.path} structured={structured} last_user={text[:80]!r}\n")

        if ":countTokens" in self.path:
            return self.reply_json({"totalTokens": 10})
        if structured:
            return self.reply_json(candidate([{"text": json.dumps(
                {"reasoning": "fake", "model_choice": "flash", "next_speaker": "user", "complexity_score": 1}
            )}]))

        if text == "__FUNCTION_RESPONSE__":
            chunks = [[{"text": "The command ran. DONECMD"}]]
        elif "RUNCMD" in text:
            chunks = [[{"functionCall": {"name": "run_shell_command",
                                         "args": {"command": "touch diri-e2e-file", "description": "fake"}}}]]
        elif "SLOW" in text:
            chunks = [[{"text": f"slow part {i} "}] for i in range(10)] + [[{"text": "SLOWDONE"}]]
        else:
            word = next(iter(re.findall(r"\b[A-Z]{4,}\b", text)), "OK")
            chunks = [[{"text": word}]]

        time.sleep(1.0)
        if ":streamGenerateContent" in self.path:
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("transfer-encoding", "chunked")
            self.end_headers()
            for parts in chunks:
                self.chunk(f"data: {json.dumps(candidate(parts))}\r\n\r\n")
                if "SLOW" in text:
                    time.sleep(0.5)
            self.chunk("")
        else:
            merged = [p for parts in chunks for p in parts]
            self.reply_json(candidate(merged))

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
