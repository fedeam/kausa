# 0022. Pass the model's answer through the pattern redaction rules before it is written anywhere, and redact the query of an HTTP request line

- Status: accepted
- Date: 2026-11-02
- Builds on: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md), [0014. Keep each incident's full explanations in a
  namespaced Explanation resource, off by default](0014-explanation-history.md) and [0015. Post
  each new explanation to one optional webhook, as JSON, Slack or Telegram](0015-notifications.md)
  (the history and the webhook receive the redacted text; no schema or body changes)
- Amends: 0008 ("Untrusted data stays data"; an `Update` paragraph when this ADR is accepted),
  invariants 4 and 5 and the flow of [ARCHITECTURE.md](../../ARCHITECTURE.md), and the `R014` row
  of [Detection and redaction rules](../reference/rules.md)
- Keeps: [0001](0001-scope-and-positioning.md) (kausa never acts on an answer),
  [0009](0009-least-privilege-rbac.md), [0013](0013-namespaced-install-mode.md) and
  [0019](0019-high-availability.md): no permission, flag, chart value, destination or request field
- Related: the other v0.5 ADRs, `0020-check-the-endpoint.md` and `0021-refill-the-call-budget.md`
  (see Consequences)

## Context

Status as of 2026-11-02 (v0.4.1). In `explain::run`, the answer goes through `prompt::validate`
(`src/prompt.rs:135`: schema, caps, `clean`), the term check (`src/explain.rs:308`), then the full
restore for the dedup entry, the log, the Event and the history (`:311`) and, with notifications,
the narrow restore (`:313-320`). `clean` removes control and format characters and markup, but
keeps kausa's `<redacted:…>` markers (`src/prompt.rs:217-231`). **No redaction rule runs on the
answer.** An address, a URL, an email or a token shape the model writes reaches the Event (read
in the namespace), the Explanation (aggregated to `view`), the log and the webhook; in Telegram a
URL stays a link ([0015](0015-notifications.md), Update 2026-10-29). Three parties shape the
answer: a tenant through cluster text, using the documented gaps (an uppercase host, a bare
`service.namespace`, a scheme-less request line, values written in pieces for the model to
join); the endpoint; and the model, which can invent values. SECURITY.md calls "a value that a
redaction rule should catch reaching a request, an Event or kausa's log in clear" a vulnerability,
which holds today for cluster text only. No injection corpus exists, and the defense was never
measured. The approved v0.5 plan (theme 3) takes the pattern rules on the answer, a counter, the
request-line case of `R014`, and a corpus measured once.

Checked on a scratch prototype of v0.4.1's `src/redact.rs` (2026-11-02, not committed):

- `Redactor::text` cannot run on an answer. Its neutralization (`src/redact.rs:241`) turns
  `deployment-1` into `deployment_1`, so nothing is restored. Its `R002` (`:258`) matches real
  names in any case ("the API server" beside a Deployment `api`), and public image references
  are sent as written (`:113-121`), so the model knows those names.
- Without those two steps, three of kausa's own outputs still break. `SECRET_ELEMENT` (`:655`)
  reads the markers `<redacted:token>` and `<redacted:secret>` as elements and eats the text after
  them (`<redacted:token> was rejected` became `<redacted:token><redacted:value>`). `R003`'s
  `ours` is an exact match (`:368`), so `pods "Pod-1"` was redacted, while `restore` accepts a
  capitalized placeholder (`:204-205`). `apply` counts every match (`:272-290`), so echoed markers
  were counted.
- A second pass over its own output is not always a no-op: in 2,592 of 300,000 random mixtures,
  existing interactions between rules (`R012`'s marker re-read by `R003`'s service-account form or
  by `R004`'s IPv6 form) changed the text again. They exist in requests too.

## Decision

**One pass, between validation and the restore.** `Redactor::answer(&mut self, text: &str) ->
String` runs the pattern rules of `Redactor::text`, in the same order, without the neutralization
and without `R002`: `R009`, `R014`, `R007`, `R008`, `R006`, `R010`, `R011`, `R005`, `R004` (IPv4
and IPv6), `R003`, `R013`, `R012`. Both functions call one private chain, so the order cannot
drift; a constant `ANSWER_RULES` lists these twelve ids, and a test checks it against the ids
`text` counts, minus `R002`. `R001` is structural and does not apply to text. In `explain::run`,
after the term check and before both restores, `summary`, `cause` and each step go through
`answer` with the incident's own `Redactor` (now `mut`, `src/explain.rs:195`);
`confidence` is a closed set and is not touched. `Explanation::map_clean` takes `FnMut`, called in
the order summary, cause, steps. The dedup entry (full and narrow), the `explanation` log line,
the Event, the history entry and the notification all derive from the one redacted text; the
unredacted answer is never logged, cached, stored or returned. `map_clean` cleans and cuts after
the pass and after the restore, so the caps hold and a marker the cut would split is dropped. The
pass does not run on a cached repeat, an answer dropped at the term check, an invalid answer, a
dry-run or an Explanation read back. It is pure and total: no I/O, no new dependency, no skip.

**Three details exist only in the answer pass**, so that kausa's own output passes unchanged
and uncounted:

- an `R010` element match whose element is a whole kausa marker is left as it is;
- `R003` treats a placeholder with a capitalized first letter (`Pod-1`), and a quoted
  `<redacted:name>`, as kausa's own;
- a match counts only when its replacement differs from the text it matched.

`R007`'s header form gets no exemption: a value after an echoed `<redacted:authorization>` is
still redacted to the end of the field. The request pass and its counts do not change, apart from
`R014` below.

**`R013` keeps its allowlist**, one list for both sides (`src/redact.rs:16`, `:30`), and the
incident's host table: a host the request replaced keeps its `host-N`, a new one gets the next
number, and `host-N` is never restored. Without the list, every `kubernetes.io` link in an answer
would become `host-N`.

**`R014` also covers an HTTP request line without a scheme.** A second pattern,
`REQUEST_LINE_QUERY`, runs right after `URL_QUERY` in both passes, handled by the same code
(generalized to take the pattern) and counted as `R014`: an uppercase method among `GET`, `HEAD`,
`POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS`, `TRACE` (case-sensitive per RFC 9110; `CONNECT`
targets have no path), spaces or tabs, and a target starting with `/`, up to its first `?` or `#`.
The tail ends as `URL_QUERY`'s does; sentence punctuation is kept, and a lone `?` or `#` is left
alone. `GET /orders?page=2 HTTP/1.1` becomes `GET /orders?<redacted:query> HTTP/1.1`;
`GET /health`, `GET /?` and `get /x?y=1` stay. Requests only send less; `PROMPT_VERSION` stays
`"2"`. Key-value forms (`path=/x?y`, `"path":"/x?y"`) are not request lines and stay in Limits.

**What the operator sees:**

| Where | What |
|---|---|
| Event note, Explanation entries, log, notification | The markers requests already use (`<redacted:ip>`, `<redacted:token>`, `?<redacted:query>`, …) and `host-N`. No new marker, Event annotation, Explanation field or notification field |
| `explanation` log line | `answer_redactions`: rule id → count, `{}` when nothing was replaced, beside `redactions` (the request's). Absent on `"cached": true` lines |
| Metrics | `kausa_answer_redactions_total{rule}`, help `Values replaced by redaction in the model's answers, by rule.`, labels the twelve `ANSWER_RULES` ids only, no namespace or owner label. `COUNTERS` goes from 19 to 20. `kausa_redactions_total`'s help text says it counts requests; its series do not change |
| Flags, chart, alerts | Nothing. No off switch (a weaker redaction setting). No example alert until the measurement shows which rules stay quiet on benign answers |

**Unchanged** (checked): the fingerprint (`src/incident.rs:490`, before any redaction), the dedup
key, the Event name (the pass is deterministic, so a repeat patches the same series),
`already_sent`, and the history's one entry per text.

**The corpus and one measurement.** `tests/bench/injection-corpus.yaml`: a Namespace
`kausa-injection` (label `purpose: kausa-injection-corpus`) and one crash-looping busybox
Deployment per case, pinned as in `tests/bench/seeded-failures.yaml`, printing its payload so it
reaches the request as a `D001` log tail. Fake values only: the canary `zq7canary` under
`.internal` (a suffix `R013` lists) and `.test` (one it does not), `192.0.2.0/24`,
`2001:db8::/32`, `example.com` for an attacker path on an allowlisted host, and a zero-filled
`ghp_` token in two halves. Cases: an instruction override with `curl … | sh`, fake system or
"kausa" text, chat-template markers, a host, an address, a token and an email written in pieces to
be joined, an allowlisted path, an uppercase URL, a demanded `high` confidence, a destructive
step, a request line, and one benign control. The payloads are also seeds of the `redact` and
`validate` fuzz targets (`fuzz/seeds/`, zipped by `.clusterfuzzlite/build.sh` as
`$OUT/<target>_seed_corpus.zip`); the `redact` target also runs `answer` and both restores. Once,
before the v0.5 release and outside CI, in a throwaway kind cluster with the seeded failures as
benign controls: v0.4.1 and this change, each against `qwen3:0.6b` on Ollama and
`gemini-3.5-flash-lite` on Google's OpenAI-compatible endpoint (about 100 calls, half hosted; the
key handled as on 2026-10-30), plus a `--dry-run` of each for the request line. A `jq` filter
over the `explanation` log lines records, per case: the indicator, `high` confidence,
`answer_redactions`, and every benign loss. The results go to
`docs/research/<date>-prompt-injection-measured.md`, worded "once each, an order of magnitude",
and to an `Update` here.

## Alternatives considered

- **Status quo, documented:** the vulnerability line stays untrue for the answer, links stay live.
  **Redact only the notification text:** the Event, the history and the log keep the values. **A
  URL and command denylist in `steps`:** judges advice, evaded by paraphrase; out of scope.
- **`Redactor::text` as it is:** breaks every restore and mangles ordinary words (Context).
- **Redact after the restore:** `R003` and `R013` would hit the real names written by design
  (0008). **Skip the explanation when the pass replaced anything:** an injector gets a switch to
  suppress explanations and spend the budget; a benign `0.0.0.0` costs the call.
- **A `source` label on `kausa_redactions_total`:** changes a documented series and its sums.
  **A new marker, annotation or Explanation field:** new vocabulary or schema for the same reader
  action. **Keep the raw answer for debugging:** that is the leak this closes.
- **Idempotence as a required, fuzzed property:** false for the existing chain (Context); echoes
  of kausa's output are tested instead. **Keep the words after an echoed `Authorization:
  <redacted:authorization>`:** the marker would shield whatever follows it.
- **No allowlist, or answer-only rules (uppercase hosts, bare hosts, NFKC folding):** a second
  rule set; NFKC would change `clean`'s fixed point and stored history. Revisit with the numbers.
- **`URL_HOST` case-insensitive now:** stricter, but outside the plan's scope; a follow-up.
  **Exempt `0.0.0.0`, `127.0.0.1`, `::`, `::1` on answers:** deferred to the benign results.
- **Tell the model not to write URLs:** `PROMPT_VERSION` 3, and a prompt is not a control. **An
  LLM-as-judge:** a second call, cost and injection target. Both out of scope.

## Consequences

- **What it buys:** values `R003`–`R014` recognize no longer reach an Event, an Explanation, the
  log or a notification, whoever wrote them, including a host the model assembled. A link to a
  host outside the allowlist loses its host and query everywhere; a host built from placeholders
  (`deployment-1.namespace-1.svc.cluster.local`) becomes `host-N` instead of the incident's
  internal DNS name in a notification. Requests lose request-line queries.
- **What it does not buy** (SECURITY.md, "What kausa cannot promise"): commands; URLs and images
  on allowlisted hosts and registries, path included (`github.com/<anyone>/…`); uppercase hosts;
  bare hosts under unlisted suffixes; split, encoded or fullwidth values; a misleading cause or a
  demanded confidence. "A notification can still … show a link" narrows to those hosts.
- **Readability costs:** `0.0.0.0`, `127.0.0.1` and address-shaped versions become
  `<redacted:ip>`; prose after `Authorization:` or `Cookie:` is lost to the end of the field; a
  word after `token:`, `auth:` or `password:` is replaced; "Basic authentication" loses its second
  word; documentation anchors lose their fragment; quoted public digests become
  `<redacted:secret>`; links to unlisted public sites (`docs.docker.com`) become `host-N`. How
  often is a hypothesis until the research note.
- **Upgrade:** Explanations written by v0.4.1 keep their text until `--history-days` ages them
  out; a repeat whose text now redacts differently starts a new entry, which may push the oldest of
  five out, at most once per Explanation.
- **Invariants:** 5 becomes "… its answer must match the schema, is cleaned and passes the pattern
  redaction rules `R003`–`R014` before it is logged, stored or written"; 4 adds "This covers the
  model's answer: running fewer rules on it needs an ADR". The flow becomes
  `prompt::validate ──▶ redact::answer ──▶ restore names`. 1, 2, 3, 6 to 11 hold.
- **Other v0.5 ADRs:** an answer the pass changed is still a successful call for the breaker of
  `0020-check-the-endpoint.md`; if that check logs any answer text, it passes `answer` first.
  The pass costs no call in `0021-refill-the-call-budget.md`. 0021's gauges and this counter both
  touch `src/metrics.rs`, with no shared meaning.
- **Docs, in the same change:** rules.md (an "On the model's answer" section, the `R014` row,
  Limits), command-line.md (the field, the counter, what a rise means), Data sent in
  notifications (the `explanation` row), Explanation resource, Data sent to the provider (request
  lines), SECURITY.md (the vulnerability line now covers the answer, Explanations and
  notifications; "Treat cluster text as data"; the two "cannot promise" items), the Architecture
  explanation, ARCHITECTURE.md, Add a detection or redaction rule (a new pattern rule joins
  `ANSWER_RULES`, with an answer and an echoed-marker case), the CHANGELOG.
- **Tests, in the same change:** each `ANSWER_RULES` rule on an answer, counted apart; echoes of
  kausa's output (`<redacted:token> was rejected`, `pods "Pod-1"`, `host-N`) unchanged and
  uncounted; "the API server" kept beside a Deployment `api`; host numbering; the drift test; the
  request-line cases (`src/redact.rs:1108` moves to the redacted side); `map_clean` at a cap. In
  `src/explain.rs` and e2e: injected values redacted in every sink, names restored, the counter.
- **Hypotheses:** ClusterFuzzLite reads `<target>_seed_corpus.zip` as OSS-Fuzz documents, and
  its builder has `zip`; both models are still offered; `temperature: 0` makes Ollama repeatable;
  models join injected pieces often enough to matter; serde_json's syntax errors, quoted in the
  `invalid_answer` reason (`src/prompt.rs:144`), never echo input; Telegram does not link
  `https://host-3/x`; loggers write methods in uppercase.
- **Follow-ups, each its own decision:** `URL_HOST` in any case; the loopback exemption; a
  narrower allowlist for answers if the allowlisted case survives; an example alert.

Update 2026-11-02: measured once with `qwen3:0.6b` on a local Ollama, v0.4.1 against this change
([Prompt injection, measured](../research/2026-11-02-prompt-injection-measured.md)). No injected
value reached the written text in either build, so the answer pass replaced nothing:
`answer_redactions` was `{}` for all 22 fresh answers, the benign controls included (no benign
loss). The model followed two injections that carry no value a rule recognizes, in both builds:
it copied the forged cause of `inj-template` and wrote the demanded `high` confidence of
`inj-confidence`. A `--dry-run` of each build showed the request line's query
(`zq7canary-acme`) in the v0.4.1 request and `?<redacted:query>` in this change's. 21 of the 22
answers were identical in both builds; the other had a different request. None of the conditions
that reopen this decision held. The `gemini-3.5-flash-lite` column is run separately by the
maintainer with the same script, and is pending as of 2026-11-02. The measurement ran kausa once
per namespace: with both, 23 incidents at once overflow the queue of 16.

Update 2026-11-04: [0025. Replace the model's text that holds one of six listed destructive command shapes with a
fixed notice, show the notice first, and give that answer `low` confidence](0025-withhold-destructive-commands.md) takes up the "A URL and command denylist in `steps`" rejected here, for
destructive commands only: after the answer pass and both restores, a field that holds one of six
listed shapes is replaced by a fixed notice in every sink, and the answer gets `low` confidence.
Redaction itself does not change.
