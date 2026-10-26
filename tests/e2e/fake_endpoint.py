"""A fake OpenAI-compatible endpoint for the end-to-end test (tests/e2e/run.sh).

It answers every POST to /v1/chat/completions with the same explanation, which names the
placeholder of the namespace so that the test can check that kausa maps it back, and appends each
request body to a file so that the test can check what left the cluster. Standard library only.
"""

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWER = {
    "summary": "e2e: a workload in namespace-1 fails.",
    "cause": "the container exits with a usage error.",
    "steps": ["check the container's arguments"],
    "confidence": "high",
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--log", required=True)
    args = parser.parse_args()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get("content-length", "0"))
            body = self.rfile.read(length)
            with open(args.log, "ab") as log:
                log.write(body + b"\n")
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
