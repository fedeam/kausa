"""Grade kausa's explanations of the seeded failures against their known causes.

It replays the requests a `kausa run --dry-run` logged against one OpenAI-compatible endpoint and
grades each answer with a fixed, deterministic rubric: no model judges another. Used by
docs/research/2026-11-02-explanation-quality.md; kept out of CI and the Dockerfile. Python's
standard library only; run it with `python3 -I`.

    python3 -I tests/bench/score.py --requests <dry-run log> --causes tests/bench/known-causes.json \
        --endpoint <base URL ending /v1> --model <name> [--no-json-schema] [--out <answers.jsonl>] \
        [--timeout <seconds>]
    python3 -I tests/bench/score.py --requests <dry-run log> --causes tests/bench/known-causes.json \
        --answers <answers.jsonl>

The first form sends each request (the first one logged per fingerprint, matched to a known cause
by namespace, owner kind, owner name and detection) to <endpoint>/chat/completions with only its
"model" replaced (and "response_format" removed with --no-json-schema), and grades the answer. The
second form grades the answers of an earlier run's --out file again, with no call: the same file
always gives the same grades. An API key is read from the KAUSA_API_KEY environment variable only,
sent as a bearer token, and never printed or written. Plain http:// is refused except to 127.0.0.1,
::1 or localhost.

The rubric, per answer:
- valid: the content parses as a JSON object (after kausa's own trimming of a ```json fence) with a
  non-empty string summary and cause, one to five non-empty string steps, and a confidence of low,
  medium or high (any case, as kausa reads it). An invalid answer fails the four criteria below.
- cause: every group of the entry's cause_all has one case-insensitive substring in summary + " " +
  cause.
- first_step: one of the entry's first_step_any is a case-insensitive substring of steps[0].
- confidence: the confidence is in the entry's confidence_ok.
- no_invented: every placeholder-like name in the answer (namespace-3, pod-2, image-1, ...) appears
  in the request's user message: the model invented no object.

Output: a table per detection id, the requests that matched no entry, and the entries with no
request (not detected). Nothing from a request, an answer or the key is printed. Exit 0 when the
run completes, 2 on a usage error.
"""

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

CONFIDENCES = ("low", "medium", "high")
PLACEHOLDER = re.compile(
    r"(?i)\b(namespace|pod|container|node|serviceaccount|registry|image|object|host|deployment"
    r"|statefulset|daemonset|replicaset|job|cronjob|persistentvolumeclaim"
    r"|horizontalpodautoscaler)-[0-9]+\b"
)
CRITERIA = ("valid", "cause", "first_step", "confidence", "no_invented")


def usage(message):
    print("score.py: " + message, file=sys.stderr)
    sys.exit(2)


def load_causes(path):
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError) as error:
        usage("cannot read the causes file: %s" % type(error).__name__)
    if not isinstance(data, dict) or data.get("version") != 1:
        usage("the causes file is not version 1")
    entries, seen = data.get("entries"), set()
    if not isinstance(entries, list):
        usage("the causes file has no list of entries")
    for i, entry in enumerate(entries):
        ok = (
            isinstance(entry, dict)
            and isinstance(entry.get("id"), str)
            and isinstance(entry.get("detection"), str)
            and isinstance(entry.get("namespace"), str)
            and isinstance(entry.get("owner"), dict)
            and isinstance(entry["owner"].get("kind"), str)
            and isinstance(entry["owner"].get("name"), str)
            and isinstance(entry.get("cause"), str)
            and isinstance(entry.get("cause_all"), list)
            and all(isinstance(g, list) and g and all(isinstance(w, str) for w in g)
                    for g in entry["cause_all"])
            and isinstance(entry.get("first_step_any"), list)
            and all(isinstance(w, str) for w in entry["first_step_any"])
            and isinstance(entry.get("confidence_ok"), list)
            and all(c in CONFIDENCES for c in entry["confidence_ok"])
        )
        if not ok:
            usage("entry %d of the causes file does not follow the schema" % i)
        if entry["id"] in seen:
            usage("entry %d of the causes file repeats an id" % i)
        seen.add(entry["id"])
    return entries


def key_of(namespace, kind, name, detection):
    return (namespace, kind, name, detection)


def load_requests(path):
    """The first `request` line per fingerprint, in the order of the log."""
    requests, seen = [], set()
    try:
        f = open(path, encoding="utf-8", errors="replace")
    except OSError as error:
        usage("cannot read the requests file: %s" % type(error).__name__)
    with f:
        for line in f:
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            if not isinstance(entry, dict) or entry.get("msg") != "request":
                continue
            owner = entry.get("owner") if isinstance(entry.get("owner"), dict) else {}
            fingerprint = entry.get("fingerprint")
            body = entry.get("request")
            if not isinstance(fingerprint, str) or not isinstance(body, dict) or fingerprint in seen:
                continue
            seen.add(fingerprint)
            requests.append({
                "fingerprint": fingerprint,
                "key": key_of(entry.get("namespace"), owner.get("kind"), owner.get("name"),
                              entry.get("detection")),
                "body": body,
            })
    return requests


def user_message(body):
    for message in body.get("messages") or []:
        if isinstance(message, dict) and message.get("role") == "user":
            content = message.get("content")
            return content if isinstance(content, str) else ""
    return ""


def parse(content):
    """kausa's reading of an answer (src/prompt.rs `validate`), with one to five steps."""
    if not isinstance(content, str):
        return None
    text = content.strip()
    for fence in ("```json", "```"):
        if text.startswith(fence) and text.endswith("```") and len(text) >= len(fence) + 3:
            text = text[len(fence):-3]
            break
    try:
        answer = json.loads(text.strip())
    except ValueError:
        return None
    if not isinstance(answer, dict):
        return None
    summary, cause, steps = answer.get("summary"), answer.get("cause"), answer.get("steps")
    confidence = answer.get("confidence")
    if not (isinstance(summary, str) and summary.strip() and isinstance(cause, str)
            and cause.strip()):
        return None
    if not (isinstance(steps, list) and 1 <= len(steps) <= 5
            and all(isinstance(s, str) and s.strip() for s in steps)):
        return None
    if not isinstance(confidence, str) or confidence.lower() not in CONFIDENCES:
        return None
    return {"summary": summary, "cause": cause, "steps": steps, "confidence": confidence.lower()}


def grade(entry, content, request_user):
    answer = parse(content)
    if answer is None:
        return dict.fromkeys(CRITERIA, False)
    said = (answer["summary"] + " " + answer["cause"]).lower()
    first = answer["steps"][0].lower()
    told = request_user.lower()
    whole = " ".join([answer["summary"], answer["cause"]] + answer["steps"])
    return {
        "valid": True,
        "cause": all(any(w.lower() in said for w in group) for group in entry["cause_all"]),
        "first_step": any(w.lower() in first for w in entry["first_step_any"]),
        "confidence": answer["confidence"] in entry["confidence_ok"],
        "no_invented": all(m.group(0).lower() in told for m in PLACEHOLDER.finditer(whole)),
    }


def check_endpoint(endpoint):
    parsed = urllib.parse.urlsplit(endpoint)
    if parsed.scheme == "https" and parsed.hostname:
        return
    if parsed.scheme == "http" and parsed.hostname in ("127.0.0.1", "::1", "localhost"):
        return
    usage("the endpoint must be https://, or http:// to 127.0.0.1, ::1 or localhost")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect is an error: the key and the request go to the endpoint given, nowhere else."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


NO_REDIRECTS = urllib.request.build_opener(_NoRedirect)


def ask(endpoint, model, body, no_json_schema, timeout, key):
    """The answer's content, or None and the reason (a status or an error name, never a body)."""
    body = dict(body)
    body["model"] = model
    if no_json_schema:
        body.pop("response_format", None)
    headers = {"content-type": "application/json"}
    if key:
        headers["authorization"] = "Bearer " + key
    request = urllib.request.Request(endpoint.rstrip("/") + "/chat/completions",
                                     data=json.dumps(body).encode(), headers=headers,
                                     method="POST")
    try:
        with NO_REDIRECTS.open(request, timeout=timeout) as response:
            data = json.loads(response.read())
    except urllib.error.HTTPError as error:
        return None, "HTTP %d" % error.code
    except (urllib.error.URLError, OSError, ValueError) as error:
        return None, type(error).__name__
    try:
        return data["choices"][0]["message"]["content"], None
    except (KeyError, IndexError, TypeError):
        return None, "no choices[0].message.content"


def main():
    parser = argparse.ArgumentParser(description="Grade explanations against known causes.")
    parser.add_argument("--requests", required=True)
    parser.add_argument("--causes", required=True)
    parser.add_argument("--endpoint")
    parser.add_argument("--model")
    parser.add_argument("--no-json-schema", action="store_true")
    parser.add_argument("--out")
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--answers")
    args = parser.parse_args()
    if args.answers and (args.endpoint or args.model or args.no_json_schema):
        usage("--answers grades an earlier run: no --endpoint, --model or --no-json-schema")
    if not args.answers:
        if not args.endpoint or not args.model:
            usage("--endpoint and --model are needed, or --answers")
        check_endpoint(args.endpoint)
    if args.timeout <= 0:
        usage("--timeout must be positive")

    entries = load_causes(args.causes)
    by_key = {key_of(e["namespace"], e["owner"]["kind"], e["owner"]["name"], e["detection"]): e
              for e in entries}
    requests = load_requests(args.requests)
    stored = {}
    if args.answers:
        try:
            with open(args.answers, encoding="utf-8") as f:
                for line in f:
                    row = json.loads(line)
                    stored[row["fingerprint"]] = row
        except (OSError, ValueError, KeyError, TypeError) as error:
            usage("cannot read the answers file: %s" % type(error).__name__)
    key = os.environ.get("KAUSA_API_KEY") or None
    out = open(args.out, "w", encoding="utf-8") if args.out else None

    unmatched, graded, matched_ids, model = [], [], set(), args.model
    for request in requests:
        entry = by_key.get(request["key"])
        if entry is None:
            unmatched.append(request)
            continue
        matched_ids.add(entry["id"])
        if args.answers:
            row = stored.get(request["fingerprint"]) or {"error": "no stored answer"}
            content, error = row.get("answer"), row.get("error")
            model = row.get("model", model)
        else:
            content, error = ask(args.endpoint, args.model, request["body"], args.no_json_schema,
                                 args.timeout, key)
        grades = grade(entry, content, user_message(request["body"]))
        graded.append((entry, grades))
        print("graded %s (%s): %s%s" % (
            entry["id"], entry["detection"],
            " ".join(c for c in CRITERIA if grades[c]) or "nothing passed",
            " [%s]" % error if error else ""), file=sys.stderr, flush=True)
        if out:
            row = {"id": entry["id"], "detection": entry["detection"],
                   "fingerprint": request["fingerprint"], "model": model}
            row.update(grades)
            row["answer"] = content
            if error:
                row["error"] = error
            out.write(json.dumps(row, sort_keys=True) + "\n")
    if out:
        out.close()

    detections = sorted({e["detection"] for e in entries})
    header = ("rule", "entries", "detected", "valid", "cause", "first_step", "confidence",
              "no_invented", "all")
    print("model: %s" % model)
    print(" | ".join(header))
    totals = [0] * (len(header) - 1)
    for detection in detections:
        mine = [g for e, g in graded if e["detection"] == detection]
        row = [
            sum(1 for e in entries if e["detection"] == detection),
            sum(1 for e in entries if e["detection"] == detection and e["id"] in matched_ids),
        ] + [sum(1 for g in mine if g[c]) for c in CRITERIA] + [
            sum(1 for g in mine if all(g[c] for c in CRITERIA))
        ]
        totals = [a + b for a, b in zip(totals, row)]
        print(" | ".join([detection] + [str(n) for n in row]))
    print(" | ".join(["total"] + [str(n) for n in totals]))
    print("requests that match no entry: %d" % len(unmatched))
    for request in unmatched:
        print("  %s %s" % (request["key"][3], request["fingerprint"]))
    missing = [e for e in entries if e["id"] not in matched_ids]
    print("entries with no request (not detected): %d" % len(missing))
    for entry in missing:
        print("  %s %s" % (entry["detection"], entry["id"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
