# 0020. Check the model endpoint on demand with `kausa check-endpoint` and `helm test`, and hold calls behind a breaker while it fails

- Status: accepted
- Date: 2026-11-02
- Builds on: [0001](0001-scope-and-positioning.md), [0009](0009-least-privilege-rbac.md) (no new
  permission), [0013](0013-namespaced-install-mode.md) (`/readyz` unchanged) and
  [0019](0019-high-availability.md) (the breaker is kept across terms)
- Amends: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md) (one fixed synthetic request, same path),
  [0010](0010-packaging.md) (two values, two chart objects), invariants 5, 6 and 7 of
  [ARCHITECTURE.md](../../ARCHITECTURE.md); each stays `accepted` with an `Update` paragraph.
- Related: [0015](0015-notifications.md) (the webhook still ignores `Retry-After`; a skip never
  notifies), `0021-refill-the-call-budget.md` (one admission function: the breaker, then the
  budget) and `0022-redact-the-answer.md` (the check never prints or writes the answer)

## Context

Status as of 2026-11-02 (v0.4.1). The first hosted-provider runs
([journal](../journal/2026-10-30-release-v0-4-0.md), lines 103-116) failed four ways, each seen
only after a crash loop settled: OpenAI `429` `insufficient_quota` (retried, then skipped), GitHub
Models `200` plain `OK`, Gemini `404` (model withdrawn), a Gemini 120 s timeout then `503`.
`CallError` has one bit, `transient` for a timeout, failed connection, `429` or `5xx`
(`src/model.rs:72`, `:95`); a `2xx` that is not JSON, or whose `choices[0].message.content` is
not a string (null included), is one permanent error (`:99-104`); the error body is only a log
`detail` (`:139-161`); the timeout is fixed at 120 s (`:49`), the retry delay at 30 s
(`src/explain.rs:105`); `Retry-After` appears nowhere. Each incident takes the budget before its
call (`src/explain.rs:225`), is refunded only when never reached (`:237-239`), and writes an
`ExplanationSkipped` on failure (`:270-279`).
`kausa run` is the only command (`src/main.rs:120-134`). `kausa.selectorLabels`
(`_helpers.tpl:13-16`) selects kausa's Pods in the NetworkPolicy (which allows the API server),
the metrics Service and the PodDisruptionBudget. Checked in kind (Kubernetes v1.36.4, Helm 3.16.1)
on 2026-11-02: install and upgrade create no test hook Pod; with `before-hook-creation`, `helm
test --logs` prints the Pod's line and exits `0` or `1`; adding `hook-succeeded` makes `--logs`
fail on a pass (`pods "…" not found`); uninstall leaves the finished Pod; a running bare Pod with
the labels of a `maxUnavailable: 1` budget over two replicas made it allow 2 disruptions.

## Decision

### `kausa check-endpoint`

- One request, on demand, never from `kausa run` or at start; no Kubernetes client, kubeconfig,
  token, retry or budget. Flags, parsed by `run`'s code: `--endpoint`, `--model` (required),
  `--api-key-file` or `KAUSA_API_KEY`, `--allow-http`, `--no-json-schema`, `--model-timeout`; any
  other, a repeated flag, an unreadable key file or `http://` without `--allow-http`: exit `2`.
- The request: a compiled-in, fabricated `D001` crash loop (Deployment `example-app`, namespace
  `example`, exit code `1`, one `BackOff` event, a log tail saying it is synthetic) through
  `context::build` and redaction, `prompt::request` (`max_tokens` 800, the configured mode), the
  same client and headers, then `prompt::validate`. A golden test pins the body; [Data sent to the
  provider](../reference/data-sent-to-the-provider.md) quotes it.
- One `endpoint check` line (`info` or `error`): `result` (`ok`, `failed`), `cause`, `class`,
  `status`; steps `connected`, `authenticated`, `model_found`, `request_accepted`, `answer_valid`
  (`true` only when proven, `false` for the failed step, else `null`); `reason`, `hint`, `detail`
  (as `run` logs it), `retry_after_seconds`, `endpoint` (scheme, host, port only), `model`,
  `json_schema`, `timeout_seconds`, `api_key` (a boolean), `latency_ms`, `tokens_in`, `tokens_out`,
  `prompt_version`. Never the answer's text. Exit `0` passed, `1` failed.

### `--model-timeout` and the chart values

- `--model-timeout <SECONDS>` on both commands: 10-600, default 120, a call's total timeout
  (connect stays 10 s); on `run` it needs `--endpoint` or `--dry-run`; `started` shows it.
- `model.timeoutSeconds: 120` and `endpointCheck.enabled: true`, read with `hasKey` (v0.4.1 values
  kept by `--reuse-values` render the defaults); anything but 10-600, or `true`/`false`, is refused.

### Causes and classes (`src/model/pure.rs`, pure, fuzzed)

`CallError.transient` becomes `CallError.cause`; `reached` stays. The first matching row wins:

| `cause` | Matches | Class | Breaker |
|---|---|---|---|
| `connection` | connect, DNS or TLS failure (still refunded) | transient | counts |
| `timeout` | no answer within `--model-timeout`, or `408` | transient | counts |
| `redirect` | any `3xx` (never followed) | configuration | opens |
| `not_an_answer` | a `2xx` not JSON, JSON with no `choices` array, or over 256 KiB | configuration | opens |
| `no_content` | a `2xx` with a `choices` array whose first message has no string `content` | rejected | none |
| `rate_limited` | `429` without a quota code | transient | counts |
| `quota` | `402`, or `429` with a code in `QUOTA_CODES` | configuration | opens |
| `key` | `401`, `403` | configuration | opens |
| `not_found` | `404`, `405`, `410` | configuration | opens |
| `too_large` | `413`, or a `4xx` with a code in `TOO_LARGE_CODES` | rejected | none |
| `rejected` | any other `4xx` (`400`, `422`, …) | rejected | none |
| `server_error` | `5xx`, and any status outside `200`-`499` | transient | counts |

- Transient: one retry, as today. Configuration: never retried, skipped at once (`endpoint`).
  `too_large` skips as `context_too_large`, `rejected` and `no_content` as `endpoint`. A completion
  that fails validation stays `invalid_answer`, a success for the breaker.
- `no_content` is the chat completion the request can cause: a structured-output `refusal`, an
  output filter (`finish_reason` `content_filter`) or a reasoning model that spends `max_tokens`
  first (`length`) may all answer with null content (hypotheses, no body captured). Its note is
  fixed per kind (refused, filtered, out of output tokens, or none), so a repeat patches the same
  Event. Only `choices[0].message.refusal` (present and not null), `choices[0].finish_reason` (a
  string of at most 64 bytes, compared with those two values) and the content's JSON type are
  read; neither text is logged, `detail` included (it is the answer's text, 0022).
- A non-`2xx` body (capped at 256 KiB) is parsed with `serde_json`, an array by its first element;
  only `error.code` and `error.type`, strings of at most 64 bytes, are compared byte for byte with
  `QUOTA_CODES = ["insufficient_quota"]` (seen 2026-10-30) and `TOO_LARGE_CODES =
  ["context_length_exceeded"]` (hypothesis, harmless: both rejected causes are neutral). A code
  joins a list only with a captured body as a fixture. Only a `cause` and a clamped delay come
  out; `detail` stays log-only. Each cause has a fixed `hint` naming the flag and the chart value.
- `Retry-After` on `429` and `503`, delta-seconds only (1-10 ASCII digits), clamped to 30-600 s;
  anything else is ignored (30 s). It sets that job's retry delay and the shortest hold when that
  answer opens the breaker. `RETRY_DELAY` stays 30 s.

### The breaker (`src/breaker.rs`, pure; state in `Shared`)

- Constants, not flags: `OPEN_AFTER` 3 consecutive transient failures, `FIRST_HOLD` 60 s,
  `MAX_HOLD` 1800 s; `now` is passed in (monotonic).
- `closed` → `open` after 3 transient failures or 1 configuration error, for `max(60 s,
  Retry-After)`; then `half_open`: the next admitted job is the one probe. A failed probe reopens
  it with the hold doubled, up to 30 min; a chat completion, valid or not, closes it and resets
  count and hold. `rejected`, `too_large` and `no_content` change nothing in any state: a probe
  that gets one leaves it `half_open`, and the next admitted job probes.
- **Admission**, one function with 0021's budget, before a free slot takes a job (a due sweep
  first). `open`: no call, budget or log read; a queued job with a fresh cache entry is served
  from it; else, with 16 queued, the slot skips the oldest with one `ExplanationSkipped`, reason
  `endpoint_unavailable`, fixed note `kausa could not explain this failure: the model endpoint is
  unavailable; this incident waited and was not explained`; else jobs wait. `closed` or
  `half_open`: 0021's rows, so a probe waits for the budget and takes one unit like any call.
- A transient first failure that leaves the breaker `open` sends the job to the **back** of the
  queue (`attempt` 1), so probes rotate; a second failure skips it (`endpoint`). While `closed`,
  it waits 30 s or `Retry-After`. A job is called at most twice per process. Retries and requeues
  obey the limit of 16 (0021); tick-time drops stay silent.
- Kept across terms in `Shared` beside `Budget` and `Dedup` (0019), so a flipped Lease cannot
  reset it; one per process; a restart resets it; the queue is dropped with the term.

### Logs, metrics, alert

- warn `endpoint unavailable` on each move into `open` (`cause`, `class`, `status`, `reason`,
  `hint`, `detail`, `failed_calls`, `probe_in_seconds`); info `endpoint available` on closing
  (`unavailable_seconds`, `failed_calls`). `explanation retried` gains `cause` and `queued`;
  `endpoint error` gains `cause` and `status`.
- Gauges `kausa_endpoint_state{state}` (`closed`, `open`, `half_open`, one at `1`; `closed`
  without an endpoint and on a standby) and `kausa_model_last_success_timestamp_seconds` (`0`
  before a chat completion); skip reason `endpoint_unavailable`. No other metric or label.
- Example alert: `max_over_time(kausa_endpoint_state{state="closed"}[20m]) == 0 and ignoring(state,
  outcome) increase(kausa_model_calls_total{outcome="error"}[45m]) > 0`.

### The chart's `helm test`

- With `endpointCheck.enabled`, `model.endpoint` and not `dryRun`: Pod
  `<fullname>-endpoint-check` (`templates/tests/`), `helm.sh/hook: test`, delete policy
  `before-hook-creation`, `restartPolicy: Never`, `activeDeadlineSeconds` timeout + 60; no
  `serviceAccountName` (the namespace's `default`), `automountServiceAccountToken: false`,
  `enableServiceLinks: false`; the Deployment's security contexts, image, pull secrets,
  `resources`, `nodeSelector`, `tolerations`, `affinity`; `check-endpoint` with its model flags;
  one volume, the API-key Secret as the Deployment mounts it. Labels `instance`, `component:
  endpoint-check`, version, `managed-by`, chart, not `app.kubernetes.io/name`: no selector built on
  `kausa.selectorLabels` matches it.
- With `networkPolicy.enabled` too, an ordinary (not hook) NetworkPolicy of the same name on those
  labels: no ingress; egress to DNS and `networkPolicy.endpoint.to`/`.ports` only.
- `NOTES.txt`: `helm test <release> --logs`, a provider call outside the budget, and the cleanup.

- **Invariant 5** gains: the endpoint's error body and `Retry-After` are untrusted; only a cause
  kausa defines and a clamped delay come from them; the body stays in kausa's log.
- **Invariant 6** gains: while the breaker is not `closed`, no call or budget but probes, one at
  a time after each hold (at least every 30 minutes while a job waits); `rejected`, `too_large`
  and `no_content`, the causes kausa classifies as the request's, never move it, and
  `invalid_answer` counts as an answer; an incident is called at most twice per process; `kausa
  check-endpoint` is not the controller, reads nothing from the cluster, and no budget counts it.
- **Invariant 7** lists `src/model/pure.rs` and `src/breaker.rs`; invariant 3 holds. **0008:** one
  fixed request with no cluster data, sent only by the check, same path. **0010:** the above.

## Alternatives

- **Status quo:** every incident pays up to two calls and one Event until a human reads the log.
- **`GET /models` preflight:** a path 0008 does not allow; it proves neither schema, quota nor an
  answer (GitHub Models' `200 OK` would pass). **A check at start-up:** an unbudgeted call per
  restart. **A fallback provider:** a second destination (0008).
- **Only the breaker:** a wrong key shows at the first incident. **Only the check:** outages cost.
- **`400` as a configuration error (the plan's wording):** a tenant shapes the request (a long
  context, a content filter) and would pause explanations for every namespace.
- **A completion with no content as `not_an_answer`, a configuration error:** a tenant's log lines
  could draw a refusal or a filter, and with its incidents as the probes it would hold every
  namespace for hours at a handful of calls, far below the budget. **As `invalid_answer`, a
  success for the breaker:** it would change v0.4.1's skip reason and outcome (`endpoint`,
  `error`) and fold three conditions into a validation failure; neutral neither opens nor closes.
- **Counting only failures that reached the endpoint (the plan's wording):** a down in-cluster
  server refuses connections; the setup 0008 recommends would keep its retries and Events.
- **The test Pod under kausa's NetworkPolicy (the plan's wording):** it needs
  `kausa.selectorLabels`, which let the budget allow both replicas' eviction (checked), put it
  behind the metrics Service and grant API-server egress. **A hook policy:** left by uninstall.
  **`hook-succeeded`:** loses the log (checked). **kausa's ServiceAccount:** a webhook keyed on
  its annotations could attach kausa's cloud identity.
- **A synthetic probe:** a call no incident asks for. **The failing job at the front:** it would
  always probe. **One 0/1 gauge:** the plan asks for both. **`state="open"` for 15 minutes:**
  resets at each probe. **`closed` absent alone:** fires while a quiet cluster sits `half_open`.
- **A second call to diagnose `response_format`, an exit code per class, breaker flags, an
  HTTP-date `Retry-After`, `error.status`** (`RESOURCE_EXHAUSTED` may mean a short limit or a
  spent day): one request is asked for, and nothing measured asks for the rest. **An Event per
  tick-time drop:** it holds the watches (0021). **`validate` fuzz seeds:** the corpus is ignored.

## Consequences

- **Cost while down:** 3 calls to open (1 for a configuration error), then probes 1, 3, 7, 15, 31
  and 61 minutes later and every 30 minutes: about 54 calls a day at most, against up to the
  budget (200 by default) today. Events: one per probe incident that fails for good and one per
  incident skipped at 16; none for those that wait; never two per incident.
- **Changed:** quota `429` and `402` are not retried; `Retry-After` can stretch a retry to 10
  minutes; incidents wait out an outage and are explained up to 30 minutes after it, reading their
  log only then; `413` skips as `context_too_large`. A fixed key needs a restart (read once,
  `src/main.rs:623-637`); added credit needs nothing. A passing `helm test` does not close the
  controller's breaker. The alert fires about 20 minutes after opening, never on a standby or a
  quiet `half_open` cluster, and resolves on closing or 45 minutes after the last failed call.
- **Threat model** (SECURITY.md): the endpoint can steer the cause and pause calls up to 30
  minutes at a time, as it could already refuse them. A tenant's content that draws a refusal, a
  filter or a long reasoning gets `no_content` and moves nothing. A new parser of untrusted text,
  pure and fuzzed (`endpoint_error`). The test Pod holds the key with no token and narrower
  egress; whoever runs `helm test` can already read the release's Secrets and create Pods, and
  pays one call.
- **Hypotheses:** content does not make a provider time out, answer `5xx` or a `2xx` with no
  `choices` array, nor a firewall `403` (rotation bounds it); the null-content shapes of a
  refusal, a filter and a spent `max_tokens`; where providers put quota codes, other codes,
  Gemini's array shape, `context_length_exceeded`, `402` for credit; `Retry-After` in seconds;
  reqwest reports DNS, TLS and connect timeouts as connect errors; failed calls are not billed;
  `activeDeadlineSeconds` covers the image pull; Argo CD and Flux skip test hooks; the alert,
  until `promtool` checks it.
- **Tests:** pure tables, the breaker on injected `Instant`s, `serve_once` per cause (a
  null-content `refusal`, `content_filter` and `length` body among them), `run.rs`
  (no call or budget while open, rotation, skip at 16, step-down), `tests/cli.rs`, render.sh, e2e.
- **Shipping:** `helm test` against Ollama in kind (pass, and `cause: not_found`); one dated
  hosted-provider run in a journal entry, first an OpenAI reasoning model if a funded key exists
  (`max_completion_tokens`, [Architecture](../explanation/architecture.md), line 201). Docs: the
  references, how-tos, Architecture, SECURITY.md, ARCHITECTURE.md, AGENTS.md, README, CHANGELOG.
