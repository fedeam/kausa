"""A logging proxy in front of Ollama for the token comparison
(docs/research/2026-10-26-tokens-compared-with-k8sgpt.md): forwards every request, and appends
one JSON line per model call with the path, the request size and the token counts Ollama reports.
Before each call it sends a one-token request with an unrelated raw prompt, so that Ollama's prompt
cache never reuses a previous request's prefix and the input counts are complete."""
import json, sys, urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer
# Usage: token_proxy.py <port> <log file> <model>. Ollama listens on its default port.
UP = "http://127.0.0.1:11434"
PORT, LOG, MODEL = int(sys.argv[1]), sys.argv[2], sys.argv[3]

def post(path, body):
    req = urllib.request.Request(UP + path, data=body, headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return r.status, r.read()

class H(BaseHTTPRequestHandler):
    # One request at a time, one per connection: the cache-clearing call must sit right before its call.
    protocol_version = "HTTP/1.0"
    def _send(self, status, data):
        self.send_response(status); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        with urllib.request.urlopen(UP + self.path, timeout=60) as r:
            self._send(r.status, r.read())
    def do_HEAD(self):
        self.send_response(200); self.send_header("content-length", "0"); self.end_headers()
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", "0")))
        model_call = self.path in ("/v1/chat/completions", "/api/generate", "/api/chat")
        if model_call:
            post("/api/generate", json.dumps({"model": MODEL, "prompt": "zz", "raw": True, "stream": False,
                                              "options": {"num_predict": 1}}).encode())
        status, data = post(self.path, body)
        if model_call:
            try:
                d = json.loads(data)
            except ValueError:  # NDJSON stream: the last line has the counts
                d = json.loads(data.splitlines()[-1])
            u = d.get("usage") or {}
            line = {"path": self.path, "request_bytes": len(body),
                    "prompt_tokens": u.get("prompt_tokens", d.get("prompt_eval_count")),
                    "completion_tokens": u.get("completion_tokens", d.get("eval_count"))}
            with open(LOG, "a") as f:
                f.write(json.dumps(line) + "\n")
        self._send(status, data)
    def log_message(self, *a): pass

HTTPServer(("127.0.0.1", PORT), H).serve_forever()
