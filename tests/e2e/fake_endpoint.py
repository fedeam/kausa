"""A fake OpenAI-compatible endpoint for the end-to-end test (tests/e2e/run.sh).

It answers every POST to /v1/chat/completions with the same explanation, which names the
placeholder of the namespace so that the test can check that kausa maps it back, and carries in its
second step a URL, an address, a token and a host the cluster never held, so that the test can check
that kausa redacts the answer before writing it anywhere (docs/adr/0022-redact-the-answer.md). It
appends each request body to a file so that the test can check what left the cluster.

A request whose body holds e2e-withhold-marker (failing-job's unknown option, which its usage error
echoes into the log tail) is answered with the same explanation plus a third step,
kubectl delete namespace --all, so that the test can check that kausa withholds a destructive
command before writing the answer anywhere and puts its notice first instead
(docs/adr/0025-withhold-destructive-commands.md). Every other request gets the two steps.

It is also the fake receiver of kausa's notifications (docs/adr/0015-notifications.md): a POST
whose path starts with /hook/ never reaches the model's request log. The first one is answered 503
and every later one 200, so that kausa retries once, and each one appends a JSON line with the
status it got, its path, its Authorization header and its body to the hook log.

It also stands in for a failing provider (docs/adr/0020-check-the-endpoint.md). With --fail-file,
while that file exists, every POST to /v1/chat/completions is answered with the status on the
file's first line, written STATUS or STATUS RETRY_AFTER (for example 401, or 503 1), with a
Retry-After header when the second number is given and an OpenAI-shaped error body. A request
whose model is e2e-missing is answered 404 with the body a provider gives an unknown model, for
the helm test that must fail with cause not_found. Both are logged like any other request, so that
each one, and each helm test, adds exactly one line to the request log and one to the time log.

With --time-log, every line appended to the request log is matched by one line in the time log:
the wall-clock second the request was received (time.time(), three decimals), comparable with the
ts of kausa's log lines because kind's nodes share the host clock (on Docker Desktop, a VM's clock
kept close to it). Both files are appended under one lock, so they always have the same number of
lines, in the same order, and the request log's line count still equals the model calls
(docs/adr/0021-refill-the-call-budget.md). Standard library only.
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
# The text in a request body that asks for the destructive third step, and that step, which kausa
# must withhold (docs/adr/0025-withhold-destructive-commands.md).
WITHHOLD_MARKER = "e2e-withhold-marker"
DESTRUCTIVE_STEP = "kubectl delete namespace --all"
# The notice kausa writes as the first step of a withheld answer, byte for byte the WITHHELD of
# src/prompt.rs and src/notification.rs (tests/chart/render.sh checks all three).
WITHHELD = (
    "Withheld by kausa: a suggested command that deletes or forces broadly. Text in this "
    "workload's logs or events may have steered this answer; check them before acting on it."
)
# The model the endpoint does not have, and the body of its 404, shaped like OpenAI's.
MISSING_MODEL = "e2e-missing"
MISSING_BODY = {
    "error": {
        "message": "The model 'e2e-missing' does not exist",
        "type": "invalid_request_error",
        "code": "model_not_found",
    }
}
# The body of every answer while the fail file exists.
FAILURE_BODY = {
    "error": {"message": "e2e: failing on purpose", "type": "e2e", "code": "e2e_failure"}
}


def failure(path):
    """The status and the Retry-After value (or None) on the first line of the file at path, or
    None while it does not exist. A line that is not one or two whole numbers raises ValueError."""
    if not path:
        return None
    try:
        with open(path, encoding="utf-8") as file:
            first = file.readline()
    except FileNotFoundError:
        return None
    words = first.split()
    if not 1 <= len(words) <= 2 or not all(w.isascii() and w.isdigit() for w in words):
        raise ValueError("the fail file's first line is not STATUS[ RETRY_AFTER]")
    return int(words[0]), (words[1] if len(words) == 2 else None)


def requested_model(body):
    """The request's JSON model, or None when the body is not a JSON object with one."""
    try:
        request = json.loads(body)
    except ValueError:
        return None
    return request.get("model") if isinstance(request, dict) else None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log", required=True)
    parser.add_argument("--hook-log", required=True)
    parser.add_argument("--time-log")
    # While this file exists, every model request is answered with the status it holds.
    parser.add_argument("--fail-file")
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

        def answer(self, status, body, retry_after=None):
            response = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(response)))
            if retry_after is not None:
                self.send_header("retry-after", retry_after)
            self.end_headers()
            self.wfile.write(response)

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
            try:
                failing = failure(args.fail_file)
            except ValueError as error:
                self.send_error(500, str(error))
                return
            if failing is not None:
                status, retry_after = failing
                self.answer(status, FAILURE_BODY, retry_after)
                return
            if requested_model(body) == MISSING_MODEL:
                self.answer(404, MISSING_BODY)
                return
            explanation = ANSWER
            if WITHHOLD_MARKER.encode() in body:
                explanation = dict(ANSWER, steps=ANSWER["steps"] + [DESTRUCTIVE_STEP])
            self.answer(
                200,
                {
                    "id": "e2e",
                    "object": "chat.completion",
                    "choices": [
                        {
                            "index": 0,
                            "finish_reason": "stop",
                            "message": {"role": "assistant", "content": json.dumps(explanation)},
                        }
                    ],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
                },
            )

        def log_message(self, format, *args):
            pass

    ThreadingHTTPServer(("0.0.0.0", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
