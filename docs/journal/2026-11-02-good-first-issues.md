# 2026-11-02 — The good first issues

The six issues kept for first-time contributors were solved by agents instead, at the maintainer's
request, one pull request each.

## Request

Solve the open good first issues (#3, #4, #5, #6, #7 and #11), and plan v0.5.

## What was done

- **#3** (#60): `Detection::is_state` and `reads_logs` are exhaustive matches, so a new detection
  fails to compile until both answers are decided. No behaviour change.
- **#4** (#61): tests tie the rule ids in the code to the tables of the rules reference, both ways;
  the detections come from a list that an exhaustive match keeps complete.
- **#5** (#62): `R011` knows npm, PyPI, Docker and more GitLab token prefixes, each from its
  issuer's documentation. Hugging Face was left out: its documentation shows only a placeholder.
- **#6** (#63): a pending incident keeps at most 100 Pod names, plus the Pods failing when it
  settles.
- **#7** (#64): repeated identical watch errors are folded into one line every 5 minutes, with a
  `folded` count; the counter still counts each one.
- **#11** (#65): a workflow checks pull request titles (Conventional Commits with a scope, at most
  64 characters) in plain shell, with the title passed through the environment and no
  permissions. It is not a required check.

## How it was checked

Each change had its own reviewer, who looked for a failing case and fixed what was found: a test
that missed a variant added at the end of the detection list, untested GitLab prefixes and token
lengths, and a wrong sentence about when a watch error is logged. After the six merges, `main`
passed `cargo fmt`, clippy and `cargo test` (428 unit tests), and CI ran on each pull request.

## Follow-ups

- `Gates::of` in `src/context.rs` and `Detection::settles` still use non-exhaustive comparisons, so
  a new detection gets a default there without a compile error.
- Dependabot titles start with an uppercase `Bump`, which the new title check rejects; it is not
  required, so the title is edited before merging.

## Later the same day: the model's answer is redacted

Request: implement [0022. Pass the model's answer through the pattern redaction rules before it is
written anywhere, and redact the query of an HTTP request line](../adr/0022-redact-the-answer.md),
with its corpus measured once.

### What was done

- `Redactor::answer` runs the twelve pattern rules of `ANSWER_RULES` (every rule of the request
  but `R002`, without the neutralization of placeholders) on the summary, the cause and each step,
  after validation and before both restores; the dedup entry, the log, the Event, the history and
  the notification all derive from the redacted text. Three details exist only on the answer, so
  that echoes of kausa's own output pass unchanged and uncounted. The values replaced are counted
  in `kausa_answer_redactions_total{rule}` and the `answer_redactions` field of the `explanation`
  log line; `kausa_redactions_total`'s help text now says it counts requests.
- `R014` also redacts the query and fragment of an HTTP request line without a scheme, on both
  sides.
- The kind test injects a step with an address, a URL with a query and a host under `.internal`
  into the fake model's answer and fails if any of them reaches an Event note, an Explanation,
  the webhook's log or kausa's `explanation` lines. The `redact` fuzz target also runs the answer
  pass and both restores; `fuzz/seeds/` holds seed corpora for `redact` and `validate`, zipped by
  `.clusterfuzzlite/build.sh`.
- The injection corpus (`tests/bench/injection-corpus.yaml`, thirteen cases) was measured once with
  `qwen3:0.6b` on a local Ollama, v0.4.1 against this change, in a throwaway kind cluster deleted
  afterwards ([Prompt injection, measured](../research/2026-11-02-prompt-injection-measured.md)).
  No injected value reached either build's answers, so the pass replaced nothing, benign controls
  included; the model copied a forged cause and obeyed a demanded `high` confidence in both. The
  request-line query left the request.
- The measurement script first ran kausa on both namespaces at once: their 23 incidents overflowed
  the queue of 16, seven were dropped and never queued again, and the run timed out. It now runs
  kausa once per namespace. The template case's indicator now matches any forged text, since the
  model copied the forged cause, not the forged summary.
- The rules reference, the command line and metrics, the data sent to the provider and in
  notifications, the Explanation resource, the security policy, both architecture pages, the
  how-to for a new rule and the changelog describe the answer pass; 0022 has an `Update` with the
  numbers.

### Decisions

- Everything as accepted in 0022. The team called no hosted provider; the maintainer's side ran
  `gemini-3.5-flash-lite` afterwards with the same script and `KAUSA_API_KEY`, on the change of
  0022 only: it followed none of the thirteen injections, and the answer pass replaced nothing.

### Hypotheses

- Supported once: `temperature: 0` made Ollama repeat its answer for the same request (21 of 22
  answers identical across the two builds; the other had a different request).
- Not supported by `qwen3:0.6b`: models assemble injected pieces often enough for the pass to
  matter (it joined none), nor by `gemini-3.5-flash-lite`, which followed no injection at all.
- Open: ClusterFuzzLite unpacks `$OUT/<target>_seed_corpus.zip` and its builder has `zip`; the
  first `cflite.yml` run on the pull request settles it.

### Follow-ups

- `URL_HOST` in any case (`R013`), and bare hosts under unlisted suffixes.
- The loopback exemption on answers, if a benign answer loses `0.0.0.0` or `127.0.0.1`.
- An example alert on `kausa_answer_redactions_total`, once the benign side shows which rules
  stay quiet.
- Check in the first `cflite.yml` log that the `redact` and `validate` seed corpora were unpacked.
