"""A fake OpenAI-compatible endpoint for the end-to-end test (tests/e2e/run.sh).

It answers every POST to /v1/chat/completions with the same explanation, which names the
placeholder of the namespace so that the test can check that kausa maps it back, and carries in its
second step a URL, an address, a token and a host the cluster never held, so that the test can check
that kausa redacts the answer before writing it anywhere (docs/adr/0022-redact-the-answer.md). It
appends each request body to a file so that the test can check what left the cluster.

It is also the fake receiver of kausa's notifications (docs/adr/0015-notifications.md): a POST
whose path starts with /hook/ never reaches the model's request log. The first one is answered 503
and every later one 200, so that kausa retries once, and each one appends a JSON line with the
status it got, its path, its Authorization header and its body to the hook log.

With --time-log, every line appended to the request log is matched by one line in the time log:
the wall-clock second the request was received (time.time(), three decimals), comparable with the
Pods' status.startTime because kind's nodes share the host clock. Both files are appended under
one lock, so they always have the same number of lines, in the same order, and the request log's
line count still equals the model calls (docs/adr/0021-refill-the-call-budget.md). Standard
library only.
"""

import argparse
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWER = {
    "summary": "e2e: a workload in namespace-1 fails.",
    "cause": "the container exits with a usage error.",
    "steps": [
        "check the container's arguments",
        "open http://192.0.2.7:8080/fix?token=not-a-real-token on zq7canary.internal",
    ],
    "confidence": "high",
}
# The values injected in the answer's second step (fake: a documentation address, a visibly fake
# token and a canary host), which no Event note, Explanation, notification or explanation log line
# may hold.
INJECTED = ["192.0.2.7", "not-a-real-token", "zq7canary"]
# The second step as kausa writes it: R004, R014 and R013 applied, the host numbered as the
# incident's request numbered it.
REDACTED_STEP = r"open http://<redacted:ip>:8080/fix\?<redacted:query> on host-[0-9]+"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--hook-log", required=True)
    parser.add_argument("--time-log")
    args = parser.parse_args()
    # The number of notifications received, under the lock that also orders the hook log.
    hooks = {"received": 0}
    hook_lock = threading.Lock()
    # Orders the request log and the time log together, so that their lines pair up.
    request_lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def hook(self, body):
            with hook_lock:
                status = 503 if hooks["received"] == 0 else 200
                hooks["received"] += 1
                entry = {
                    "status": status,
                    "path": self.path,
                    "authorization": self.headers.get("authorization"),
                    "body": body.decode("utf-8", errors="replace"),
                }
                with open(args.hook_log, "a", encoding="utf-8") as log:
                    log.write(json.dumps(entry) + "\n")
            self.send_response(status)
            self.send_header("content-length", "0")
            self.end_headers()

        def do_POST(self):
            received = time.time()
            length = int(self.headers.get("content-length", "0"))
            body = self.rfile.read(length)
            if self.path.startswith("/hook/"):
                self.hook(body)
                return
            with request_lock:
                with open(args.log, "ab") as log:
                    log.write(body + b"\n")
                if args.time_log:
                    with open(args.time_log, "a", encoding="utf-8") as times:
                        times.write(f"{received:.3f}\n")
            if self.path != "/v1/chat/completions":
                self.send_error(404)
                return
            response = json.dumps(
                {
                    "id": "e2e",
                    "object": "chat.completion",
                    "choices": [
                        {
                            "index": 0,
                            "finish_reason": "stop",
                            "message": {"role": "assistant", "content": json.dumps(ANSWER)},
                        }
                    ],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
                }
            ).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

        def log_message(self, format, *args):
            pass

    ThreadingHTTPServer(("0.0.0.0", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
