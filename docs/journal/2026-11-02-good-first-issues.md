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
## Later the same day: smaller list pages

The request: choose the list page size of the three watches by measuring it, the related work of
[0021](../adr/0021-refill-the-call-budget.md).

- `tests/bench/measure-start.sh` now reports the peak memory and the list requests, and
  `tests/bench/thousands-of-pods.py --forged N` writes zero-replica Deployments with a 256,000-byte
  annotation each.
- In a throwaway kind cluster with 10,000 Pods at the largest scheduling record and then 500 forged
  Deployments, pages of 500, 250, 100 and 50 were measured ([List
  pages](../research/2026-11-02-list-pages.md)). With the forged page, the peak of the macOS
  binary fell from about 600 MiB with 500 to 161 to 170 MiB with 50, for ten times the list
  requests and no slower start.
- Decision (no ADR, by the rule set before measuring): `LIST_PAGE_SIZE` is 50 for the Pod, Event
  and Deployment watches. No page met the 144 MiB target (75% of the chart's 192Mi), so 50, the
  lowest peak, was taken and the margin is flagged in the note and in `SECURITY.md`.
- Follow-up: measure the chart's Pod on Linux (`memory.peak`) with a forged page, to see whether
  192Mi keeps its margin there.

## Later the same day: refill the call budget

The request: implement [0021. Refill the hourly call budget at a steady rate from a start burst of
three, and let incidents wait for it in the queue, so that a restart does not refill
it](../adr/0021-refill-the-call-budget.md), with the smaller list pages above as its related work.

- `src/budget.rs`: the hourly cap is a refill bucket under the 60-minute and 24-hour windows. A
  process starts with 3 calls (`N` if fewer), gains one every `3600/N` seconds and holds at most
  `N`; `check` answers `Now`, `Wait { until }` or `Never` and changes nothing, `take` stays the
  authority, `refund` gives back the call and its credit, and `start_term` cuts the credit to the
  burst at a takeover. The state is one `Instant`, with checked arithmetic that refuses a call
  rather than panic. A seeded schedule test checks the stated bound in exact nanoseconds.
- `src/run.rs`: the explanation slot admits a job through `allowance` and `admit`. While the budget
  waits, jobs wait in the queue, a repeat the cache answers passes them, and the oldest of a full
  queue of 16 is skipped with an `ExplanationSkipped` Event. New incidents and delayed retries join
  through `enqueue`, so the queue never holds more than 16. `explanations waiting` is logged once
  per waiting period, and `explanation dropped` gains `waiting_for_budget`.
- `src/explain.rs`: a job the budget refuses is skipped before its log is read.
- `src/metrics.rs`: `kausa_budget_calls_remaining{window}` and `kausa_explanation_queue_length`,
  rendered only once set (no budget gauge in dry-run or detect-only).
- `--help`, the chart's comments and `NOTES.txt` describe the refill; no flag, value or default
  changed. The e2e test installs with `budget.callsPerHour=3600`, and the cluster scope's run adds
  a case that deletes kausa's Pod every 60 s for 10 minutes at `callsPerHour=360` and checks the
  bound from the fake endpoint's receive times. Its arithmetic was checked offline against
  synthetic schedules (a v0.4.1-like one fails it); the case itself runs only in CI.
- The documentation (CHANGELOG, Architecture, SECURITY.md, Command line and log format, Helm chart,
  Install kausa with Helm with two example alerts, ARCHITECTURE.md's codemap) states the bound
  `c(s) + 3K + T/I` and the advice `callsPerHour <= callsPerDay / 24`. 0019's `Update` paragraph
  came with the ADR's acceptance. Two stale items were fixed on the way: the takeover times and
  the metrics flood, measured on 2026-10-29, were still called open items, and SECURITY.md still
  said clusters over about a hundred Pods were untested.

Decisions, all in 0021: a start burst of 3; one admission function, so that 0020's breaker goes in
front of the budget in `allowance` and shares the queue rules; drops at the tick stay silent
(logged and counted, no Event, so no write can hold the loop); delayed retries obey the limit of
16. The e2e job of the cluster scope took about 7 minutes on `main` on 2026-11-02, so the new case
(about 11 to 12 minutes) fits its 30-minute timeout, which stays.

Checks: `cargo fmt`, clippy with `-D warnings`, `cargo test` (462 unit tests, 56 command-line
tests), `cargo deny`, `helm lint --strict` and `tests/chart/render.sh` pass locally. The kind tests
were not run locally; CI runs them on the pull request.

Hypotheses still open, none checked here:

- H1: kubelet's crash back-off caps at one restart per 5 minutes; only the worked example (66
  calls an hour at the defaults) uses it.
- H2: terms do not overlap (0019's timing hypotheses with the Lease, `strategy: Recreate`
  without it); the bound rests on it. The e2e case deletes the Pod with the default wait, so its
  lives do not overlap by construction.
- H3: a start delay of up to 5 s (one tick) and a burst of 3 suit operators: to be sampled from
  both gauges in the soak planned against the v0.5 release candidate.
- H4: whether the 2026-10-29 out-of-memory loop ever made a call was not recorded; nothing here
  depends on it.
- H5: the `pod` label in the example alert `KausaDailyCallBudgetSpent` depends on the scrape
  configuration.
## Later the same day: detection coverage and a known-cause corpus

**Request.** One of the four v0.5 themes the maintainer approved on 2026-11-02, without an
ADR: close the detection gaps for a Job's other failure reasons, a CronJob that cannot create its Jobs,
`ErrImageNeverPull` and a container that cannot start, and build a known-cause corpus that grades
explanations.

**What was done.**

- Each candidate was reproduced first on kind (Kubernetes v1.36.4) and read in the Kubernetes
  source at `v1.36.4`. `D008` matches the five reasons of a Job's `Failed` condition, `D011` a
  CronJob's `FailedCreate` (renamed `workload cannot create pods or jobs`), `D002`
  `ErrImageNeverPull`, and `D004` `RunContainerError` and a `CrashLoopBackOff` whose last
  termination is `StartError` (renamed `container cannot be created or started`). No id, permission
  or request field was added; `PROMPT_VERSION` and the prompt are unchanged.
- `R003` redacts the container and the Pod of the Job controller's two `podFailurePolicy` note
  formats: a `D008` incident gathers no Pod, so neither is a registered name and the container's
  name would have reached the model.
- The fingerprint replaces the Pod named in a `PodFailurePolicy` note (generated names, as for
  `D011`), and uses the container's name for a start failure, whose sample alternates between the
  runtime's message and the back-off message.
- The kind test seeds `policy-job`, a Job failed by its `podFailurePolicy` with retries left, and
  checks its incident, its redacted request and its notification. It does not seed the CronJob: a
  `count/jobs.batch` quota in its namespace would refuse the test's other Jobs, a namespace of its
  own would change the scope lists and RBAC assertions, and its first event can wait 60 seconds.
- `tests/bench/` got one seeded failure per detection (20 entries), their causes and a keyword
  rubric in `known-causes.json`, committed before any model ran, and `score.py`, which replays a
  dry-run's requests against an endpoint and grades the answers with no model as judge.
- The corpus ran once: one kind cluster with its own kubeconfig (the shared Docker disk had 4.9 GB
  free; the default kubeconfig's checksum was the same before and after; the cluster and its
  kubeconfig were deleted and nothing was pruned), `kausa run --dry-run`, then Ollama 0.9.2 on the
  same machine. Every seeded failure gave its request. `qwen3:4b` passed every criterion on 15 of
  20 entries, `qwen3:0.6b` on 10 ([Explanation quality of local models against known
  causes](../research/2026-11-02-explanation-quality.md)). No hosted model was called.

**Decisions.**

- No ADR: rules and a corpus, with no new permission, flag or field (the `D009`-`D013` precedent).
- `D011` keyed to a CronJob does not wait for the settle window. On kind the CronJob controller
  wrote one series of 14 events in 41 seconds for the first scheduled time and then one new event
  per scheduled time, so no event would ever settle under the 60-second window, and an hourly
  CronJob's next event comes after the last one is stale. A Job of a CronJob whose Pods are refused
  is keyed to the CronJob, so it is an incident at once too.
- Corrected in review the same day: the exemption follows the event's `regarding`, not the key. A
  Job of a CronJob whose Pods are refused waits for the window again, as before this branch: the
  Job controller keeps retrying while the Job is active, and a refusal that fixes itself must not
  become an incident.
- The threat model does not change: a forged `FailedCreate` about a CronJob is an incident at once,
  as a forged failed-Job event already was, and costs one call within the budget.
- `qwen3:4b` is the minimum local model the how-to recommends, on one run.

**Hypotheses checked.**

- Confirmed: the Job controller writes every `Failed` reason as a Warning on the Job; the CronJob
  controller writes `FailedCreate` on the CronJob; a CronJob in `regarding` is its own top-level
  owner with no change to `src/run.rs` or `src/owner.rs` (unit tests, and the corpus run's `report`
  incident); the log of a `PodFailurePolicy` Job's failed Pod is read (the corpus request carried
  it); the `PodFailurePolicy` fingerprint is stable across reruns (unit tests).
- Not checked: why the kubelet reports `RunContainerError` rather than `CrashLoopBackOff` most of
  the time; a Job whose name is truncated in its Pods' names (its fingerprint may change per
  rerun).

**Follow-ups.**

- Done the same day: the hosted column of the research note, `gemini-3.5-flash-lite` through
  `score.py --endpoint --model` with `KAUSA_API_KEY`: 16 of 20 entries passed everything, one
  answer lost to the free tier's `429`.
- The human pass over the answers, pending for the maintainer; an agent read-through found three
  rubric gaps, kept as committed.
- A hints ADR only if more runs confirm that small models miss known signatures and invent objects
  for incidents with no Pod.

## Later the same day: check the endpoint and hold calls behind a breaker

**Request.** Implement [0020. Check the model endpoint on demand with `kausa check-endpoint` and
`helm test`, and hold calls behind a breaker while it fails](../adr/0020-check-the-endpoint.md) on
top of the integrated 0021 and 0022 work, in two tracks (code, chart) merged into one branch.

**What was done.**

- `src/model/pure.rs`: the causes (`connection`, `timeout`, `redirect`, `not_an_answer`,
  `no_content` of four kinds, `rate_limited`, `quota`, `key`, `not_found`, `too_large`,
  `rejected`, `server_error`), their classes and hints, `classify` by status and by `error.code` /
  `error.type` (strings of at most 64 bytes, an array by its first element), `completion` and
  `retry_after` (delta-seconds only, clamped to 30-600 s). Pure, no `crate::` import, fuzzed by
  the new target `endpoint_error`, and run by ClusterFuzzLite on pull requests that change it.
- `src/model.rs`: `CallError` carries a cause, a status and a delay instead of `transient`;
  `--model-timeout` sets the total timeout; `Retry-After` is read before the body.
- `src/breaker.rs`: the pure breaker (3 transient failures or 1 configuration error; holds of 60 s
  doubling to 1800 s; `Retry-After` raises a hold and never compounds into it).
- `src/explain.rs` and `src/run.rs`: `Shared.breaker`, kept across terms; the call is recorded
  before the term check; `explain::run` returns `Next` (done, retry, requeue at the back);
  `allowance` maps an open breaker to 0021's gate-holds branch and `refusal` puts
  `endpoint_unavailable` in front of the budget's rows; `endpoint unavailable` and `endpoint
  available` once per transition; `explanation retried` gains `cause` and `queued`, `endpoint
  error` `cause` and `status`.
- `src/metrics.rs`: `GAUGES` grows from 3 to 5 (`kausa_endpoint_state`,
  `kausa_model_last_success_timestamp_seconds`, last); `COUNTERS` stays at 20.
- `src/check.rs` and `src/main.rs`: `kausa check-endpoint`, the six model flags parsed by one
  helper for both commands, `--model-timeout`, `model_timeout_seconds` in `started`, and
  `prompt::MAX_TOKENS` (800) shared by both.
- The chart: `model.timeoutSeconds`, `endpointCheck.enabled`, the `helm test` Pod
  `<fullname>-endpoint-check` and its own NetworkPolicy, `NOTES.txt`; `kausa.rules`, `rbac.yaml`
  and `leader-election.yaml` unchanged.
- The kind test: `helm test --logs` passes in every run and fails with `cause` `not_found` on a
  missing model; the cluster-scope, one-replica run adds the breaker phase against a fake
  endpoint answering `401`.
- The documentation listed in the ADR's Consequences, and `Update` paragraphs on 0008 and 0010
  recording the implementation.
- The merge of the chart track had one conflict, the comment at the top of
  `.github/workflows/cflite.yml`; the chart track's wording was kept.

**Decisions** (0020 holds the design; these are the choices made while building it):

- The request bodies are pinned by golden files under `tests/golden/`
  (`check-endpoint-request-json-schema.json`, `check-endpoint-request-instructions.json`),
  compared as parsed JSON by a unit test and by `tests/cli.rs` against what a local fake server
  received, rather than by fixtures inside `src/check.rs`.
- `waiting_for_budget` on `explanation dropped` stays about the budget only: it is `false` while
  the breaker holds the slot. `explanations waiting` carries the reason of the gate that holds,
  `the hourly call budget is spent` or `the model endpoint is unavailable`, with the end of the
  hold as `next_call_in_seconds`.
- `kausa_model_last_success_timestamp_seconds` is rendered only with a model client
  (`--endpoint`, not `--dry-run`), like `kausa_budget_calls_remaining`; `kausa_endpoint_state` is
  set to `closed` at start in every mode.
- The breaker's e2e phase runs once, in the `KAUSA_E2E_SCOPE=cluster` `KAUSA_E2E_REPLICAS=1` job,
  to keep the other two jobs within their timeouts.
- The fabricated incident's `EXAMPLE_URL` reaches the model as `namespace-1_URL`: `R002` replaces
  the namespace `example` in any ASCII case. Kept: it shows the redaction at work, and the golden
  files pin it.

**How it was checked.**

- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D
  warnings` and `cargo test --locked --all-features` (579 unit tests, 75 command-line tests) pass
  on the merged branch; `helm lint --strict charts/kausa --set rbac.scope=cluster` and `bash
  tests/chart/render.sh` pass; the eight `RUN` blocks of the `Dockerfile` that call `helm` were
  run by hand with the local Helm and macOS tools (their `/tmp` files moved to a scratch
  directory) and pass; `bash -n tests/e2e/run.sh` and `python3 -I -m py_compile
  tests/e2e/fake_endpoint.py` pass. `docker build --target ci` and the kind tests were not run
  locally; CI runs them on the pull request.
- `kausa check-endpoint` against Ollama 0.9.2 on the same machine (started for the check and
  stopped after it; no hosted provider): `qwen3:0.6b` and `qwen3:4b` passed (1.6 s and 4.0 s,
  about 510 tokens in); a model that is not pulled failed with `not_found` (`model_found`
  `false`); the base URL without `/v1` gave `not_found` too; a closed port gave `connection`
  (`connected` `false`); `qwen3:0.6b` with `--no-json-schema` gave `invalid_answer`, its answer
  not being JSON. The lines are quoted in [Command line and log
  format](../reference/command-line.md#the-endpoint-check-line).
- The ADR's shipping check (`helm test` against Ollama in kind) and a `promtool check rules` of
  the example alert in a pinned container were **not run**: the shared Docker disk had 3.8 GB
  free, under the 4 GB this session allows itself. No cluster was created and `~/.kube/config`
  was not touched.

**Hypotheses still open** (none confirmed or refuted here, beyond what the Ollama runs show):

- Where providers put quota codes (`code` or `type`), other providers' codes, Gemini's array
  shape, `context_length_exceeded`, `402` for credit, and `Retry-After` in seconds.
- The null-content shapes of a structured-output refusal, a `content_filter` finish and a
  reasoning model that spends `max_tokens` (no body captured).
- That reqwest reports DNS, TLS and connect timeouts as connect errors (a refused connection
  does, in the unit tests and against the closed port above).
- That content does not make a provider time out, answer `5xx`, or a `2xx` with no `choices`
  array, nor a firewall `403`; that failed calls are not billed.
- That `activeDeadlineSeconds` covers the image pull; how Argo CD and Flux treat `helm.sh/hook:
  test` objects, to settle before `endpointCheck.enabled` keeps its default `true`.
- The example alert's PromQL, until `promtool` runs on it.

**Follow-ups.**

- The ADR's shipping checks: `helm test` against Ollama in kind (a pass, and `not_found` on a
  wrong model), when the Docker disk has room; the kind jobs of the pull request cover the same
  path against the fake endpoint.
- One dated `kausa check-endpoint` run against a hosted provider with a key the maintainer gives
  and revokes, first an OpenAI reasoning model if a funded key exists (`max_completion_tokens`);
  never run by an agent here.
- `promtool check rules` on the example alert `KausaModelEndpointFailing`.
- Argo CD's (and Flux's) treatment of the test hook, before the release.
## Later the same day: the budget case counted a call in the wrong life

The kind job of the cluster scope failed in the budget case: `life 2: request 5 came 11.836 s
after its start; at most 4 may`.

- **Cause: the test, not the bucket.** A life started at its Pod's `status.startTime`, which the
  API gives in whole seconds and the test cut down, so the new life began up to a second before
  the replacement Pod existed, and could begin before the old one was deleted. The old process
  stops about 50 ms after the delete (SIGTERM, then `stopped`), so its last call, if it came in
  that second, was counted in the new life, on top of the new process's own burst.
- **Reproduced locally**, in kind on Docker Desktop with the case's settings and the old
  attribution: life 7's fifth request came 16.690 s after its start, where 4 may. The old life had
  called 0.096 s after the new Pod's start as cut, 0.59 s before the delete. Over 9 deletions,
  the old process stopped 0.04 to 0.74 s after the next Pod's start as cut, and 0.41 to 0.71 s
  before the next process logged `started`. Counted from each process's `started` line, no life
  went over the bound. CI kept no request times, so that its failure had this cause is inferred
  from the reproduction, not seen.
- **The fix** (`tests/e2e/run.sh`): each life is counted from its process's `started` line,
  logged before the process builds its budget. The log of every life but the last is followed
  until the process ends, and the check fails if a life has no `stopped` line or stopped after
  the next one started: H2 above is now checked, not assumed. The 1 s margin stays, now for the
  node's clock against the host's (on Docker Desktop, a VM's). The bound is stricter than before,
  since a process starts after its Pod.
- **The code follows 0021:** credit accrues from the process start whatever the phase, so a life
  may start 4 calls at once 15 s in (a credit of 3 + 1.5); the test allows `3 + floor(r / 10 s)`.
- **Checks:** `cargo fmt`, clippy with `-D warnings`, `cargo test`, `bash -n` and `shellcheck` pass.
  The kind test of the cluster scope passed locally on Docker Desktop with the fix: 88 requests in
  610 s over 10 starts, at most 90, and every process logged `stopped` 0.41 to 0.53 s before the
  next one logged `started`.
