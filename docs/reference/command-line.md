# Command line and log format

The commands and flags of the `kausa` binary, its exit codes, and the JSON lines it writes.

Status as of 2026-10-30: what v0.4.1 does, with the namespaced mode, the explanation history and
the notifications of v0.3.0 (`--namespace`, `--history`, `--history-days` and the `--notify-*`
flags) and the Deployment watch and the standby of v0.4.0 (`--no-rollouts` and
`--leader-lease`). `kausa run` detects and groups failures and logs incidents; with `--endpoint`
it explains each one within a call budget, reuses explanations of repeated incidents, and writes the explanation as an Event
on the failing workload; with `--dry-run` it logs the request instead; with `--metrics-address` it
serves Prometheus metrics ([0001. Build a small, read-only explainer that writes one redacted
explanation per incident](../adr/0001-scope-and-positioning.md)); with `--namespace` it watches
only the namespaces given ([0013. Offer a namespaced install mode: one watch pair and one Role per
listed namespace, and nothing cluster-wide](../adr/0013-namespaced-install-mode.md)); with
`--history` it also keeps each explained incident's full explanations in an Explanation object in
its namespace ([0014. Keep each incident's full explanations in a namespaced Explanation resource,
off by default](../adr/0014-explanation-history.md)); with `--notify-url-file` it also posts each
new explanation to one webhook ([0015. Post each new explanation to one optional webhook, as JSON,
Slack or Telegram](../adr/0015-notifications.md)). v0.4.0 adds a watch on Deployments
for stalled rollouts (`D014`), on unless `--no-rollouts` ([0018. Explain a Deployment past its
progress deadline (`D014`) from a Deployment watch that is on by default, and add nothing for
StatefulSets or DaemonSets](../adr/0018-stuck-rollouts.md)), and `--leader-lease`, which makes a
replica act only while it holds a Lease, for a standby beside it ([0019. Offer an optional standby
replica that takes over through a chart-created Lease, elected by kausa's own
code](../adr/0019-high-availability.md)). Since v0.5 (not released as of 2026-11-02), `kausa
check-endpoint` sends one synthetic request to the model endpoint on demand, `--model-timeout`
sets a call's timeout, and `kausa run` holds its calls behind a breaker while the endpoint fails
([0020. Check the model endpoint on demand with `kausa check-endpoint` and `helm test`, and hold
calls behind a breaker while it fails](../adr/0020-check-the-endpoint.md)).

## Commands

| Command | What it does |
|---|---|
| `kausa run [OPTIONS]` | Runs the controller until `SIGTERM` or Ctrl-C |
| `kausa check-endpoint [OPTIONS]` | Sends one synthetic request to the model endpoint and writes one `endpoint check` line ([kausa check-endpoint](#kausa-check-endpoint)) |
| `kausa -h`, `kausa --help` | Prints the help |
| `kausa -V`, `kausa --version` | Prints `kausa <version>` |

Options of `kausa run`:

| Option | What it does |
|---|---|
| `--settle-seconds <N>` | How long a failure has to persist before it is an incident, from `0` to `3600`; `60` by default. A failed Job (`D008`), and a CronJob that cannot create its Jobs (a `D011` event about the CronJob itself), is an incident at once, whatever the value; a Job of a CronJob that cannot create its Pods waits like any other event |
| `--namespace <NAME>` | Watch only this namespace: one Pod watch, one Warning event watch and, unless `--no-rollouts`, one Deployment watch in it. Repeat it for more, one name each time, up to 32; the order does not matter. Without it, kausa watches the whole cluster (one watch of each kind across all namespaces). The scope comes only from this flag, never from the kubeconfig's context or the service account's namespace |
| `--no-rollouts` | Do not watch Deployments, so a rollout past its progress deadline (`D014`) is not explained. The watch is on by default and needs `list` and `watch` on `deployments`; turn it off when kausa's role does not grant them. Independent of `--endpoint` and `--dry-run` |
| `--endpoint <URL>` | The base URL of an OpenAI-compatible endpoint, such as `https://api.example.com/v1` or `http://127.0.0.1:11434/v1` for Ollama. Each incident's redacted context is sent to `<URL>/chat/completions` and the answer is logged as an explanation. Needs `--model` |
| `--model <NAME>` | The model to ask |
| `--api-key-file <PATH>` | A file whose content (trimmed) is the API key, sent as `Authorization: Bearer`. Without it, the environment variable `KAUSA_API_KEY`; without both, no key. Never an argument |
| `--allow-http` | Allow an `http://` endpoint, for one on the same machine, in the cluster or in the same private network. With `--dry-run` the endpoint is not used, so its scheme is not checked |
| `--dry-run` | Build each request and log it instead of sending it; no endpoint needed |
| `--no-logs` | Do not read container logs (the request has no `log_tail`) |
| `--no-json-schema` | Do not send `response_format`, for servers without structured output; the answer is validated all the same |
| `--model-timeout <N>` | A model call's total timeout in seconds, a whole number from `10` to `600`; `120` by default. Connecting has its own 10 seconds. Needs `--endpoint` or `--dry-run` (with `--dry-run` it is not used); the `started` line shows it as `model_timeout_seconds` (`null` without a model client) |
| `--no-events` | Do not write Events; the explanations are only logged. Events are never written in dry-run |
| `--history` | Keep each explained incident's explanations (fresh or from the deduplication cache) in an Explanation object named `kausa.<fingerprint>` in its namespace, after the Event and independent of it, and delete old ones: needs the CustomResourceDefinition `explanations.kausa.fedeam.com` ([Explanation resource](explanation-resource.md)). Needs `--endpoint`; not with `--dry-run`. With `--no-events`, kausa writes the history only |
| `--history-days <N>` | Delete Explanations, and drop entries, last written more than `N` days ago, from `1` to `30`; `7` by default. Needs `--history` |
| `--notify-url-file <PATH>` | Post each new explanation, after its Event, to the webhook whose URL is the content of this file (trimmed), read once at start. The URL is a secret: no flag value or environment variable form, and it is never logged. Needs `--endpoint`; not with `--dry-run`. What is sent: [Data sent in notifications](data-sent-in-notifications.md) |
| `--notify-format <FORMAT>` | The body: `json` (the default), `slack` or `telegram` |
| `--notify-telegram-chat-id <ID>` | With `telegram` only, and required with it: the chat, an integer that is not `0` (a leading `-`, no `+`) or `@` and a channel's username (a letter, then 4 to 31 letters, digits or `_`). Not a secret |
| `--notify-token-file <PATH>` | With `json` only, optional: a file whose content (trimmed) is sent as `Authorization: Bearer <token>` |
| `--notify-allow-http` | Allow an `http://` webhook, for one in the cluster or the same private network; separate from `--allow-http` |
| `--max-calls-per-hour <N>` | Model calls allowed in any hour, `30` by default; `0` allows none. Each start allows 3 at once (`N` if fewer), then one every `3600/N` seconds, up to `N` saved; with `--leader-lease`, each takeover also starts with at most 3 ([0021](../adr/0021-refill-the-call-budget.md)). Help: `Model calls allowed in any hour (default 30); each start allows 3 at once, then one every 3600/N seconds` |
| `--max-calls-per-day <N>` | Model calls allowed in any 24 hours, per process, `200` by default. A restart starts a new process: for the daily cap to hold across restarts, up to the bursts of 3, set `--max-calls-per-hour` to at most a 24th of it |
| `--max-input-tokens <N>` | Cap on a request's estimated input tokens (a quarter of its bytes), `6000` by default: the context is cut to fit, and an incident that still does not fit is skipped |
| `--dedup-hours <N>` | How long an incident's explanation is reused when the same incident (same fingerprint) comes back, `6` by default; `0` turns the reuse off |
| `--metrics-address <ADDR>` | Serve `/metrics`, `/healthz` and `/readyz` on this IP address and port, for example `[::]:9090` in a Pod (IPv4 and IPv6 on Linux) or `127.0.0.1:9090` on a machine; not served by default |
| `--leader-lease <NAMESPACE>/<NAME>` | Act only while holding this Lease, and wait as a standby otherwise: for two replicas, of which one acts ([Leader election](#leader-election)). The Lease must exist (the Helm chart creates it with `replicaCount: 2`); kausa needs `get` and `patch` on it and never creates it. Needs `POD_NAME`, the identity this replica holds the Lease with. Works in every mode, log-only and `--dry-run` included |

Without `--endpoint` or `--dry-run`, kausa only logs incidents. The Events are written as
`kausa` with the reporting instance from the environment variable `POD_NAME` (set it from the
Pod's name with the downward API, as the Helm chart does), or `kausa` (only without
`--leader-lease`, which requires `POD_NAME`; the holder of the Lease and the reporting instance of
its Events are then the same name). `--model`, `--api-key-file`, `--allow-http`,
`--no-json-schema` and `--model-timeout` alone are usage errors: `--model, --api-key-file,
--allow-http, --no-json-schema and --model-timeout need --endpoint or --dry-run`. A
`--model-timeout` that is not a whole number from 10 to 600 (`9`, `601`, `abc`, `-5`, `30.5`) is
refused when the flag is read: `--model-timeout takes a whole number of seconds from 10 to 600,
not '<value>'`. On `kausa run`, a model flag given twice keeps its last value.

The usage errors of `--leader-lease`: the first five when the flag is read, the last two once
every argument has been read and only with the flag, before the other checks of the flags:

| Case | Message after `error: ` |
|---|---|
| No value, or an empty one | `--leader-lease needs <NAMESPACE>/<NAME>` |
| Given twice | `--leader-lease is given twice` |
| Not exactly one `/`, or an empty part | `--leader-lease takes <NAMESPACE>/<NAME>, not '<value>'` |
| A namespace that is not a namespace name (a DNS-1123 label) | `--leader-lease: '<namespace>' is not a namespace name` |
| A name that is not a Lease name (a DNS-1123 subdomain: 1 to 253 characters, dot-separated parts of lowercase letters, digits and `-`, each starting and ending with a letter or digit) | `--leader-lease: '<name>' is not a Lease name` |
| `POD_NAME` unset or empty | `--leader-lease needs POD_NAME, the name this replica holds the Lease with (the chart sets it from the Pod's name)` |
| `POD_NAME` longer than 253 bytes | `POD_NAME is longer than 253 bytes` |

`POD_NAME` is compared byte for byte with the Lease's holder, never checked as a name.

The usage errors of `--namespace`, checked in argument order before kausa connects (each name
first as a name, then against the earlier ones; the count last):

| Case | Message after `error: ` |
|---|---|
| No value, or an empty one | `--namespace needs a value` |
| Not a namespace name (a DNS-1123 label: 1 to 63 lowercase letters, digits and `-`, starting and ending with a letter or digit) | `--namespace takes a namespace name (lowercase letters, digits and '-', starting and ending with a letter or digit, at most 63 characters), not '<value>'` |
| A name given twice | `--namespace '<name>' is given twice` |
| More than 32 | `--namespace is given <n> times; kausa watches at most 32 namespaces (watch the whole cluster instead)` |

The usage errors of the history, after those of `--namespace` and before those of `--endpoint`
and `--model` (so `--history --endpoint <URL>` without `--model` still says `--endpoint needs
--model`):

| Case | Message after `error: ` |
|---|---|
| `--history-days` without a value | `--history-days needs a value` |
| `--history-days` with a value that is not a whole number from 1 to 30 (`0`, `31`, `abc`, `-1`, `7.5`), checked when the flag is read | `--history-days takes a whole number of days from 1 to 30, not '<value>'` |
| `--history-days` without `--history` | `--history-days needs --history` |
| `--history` with `--dry-run`, with or without `--endpoint` | `--history is not used with --dry-run: no explanation is written` |
| `--history` without `--endpoint` | `--history needs --endpoint` |

The usage errors of the notifications: the first two rows when the flag is read, the others
after the errors of the history and before those of `--endpoint` and `--model`, in this order. No message carries the content of either file, and no
test message is sent at start:

| Case | Message after `error: ` |
|---|---|
| A `--notify-*` flag that takes a value, without one (or, but for the two file flags, with an empty one), checked when the flag is read | `<flag> needs a value` |
| `--notify-format` with another value, checked when the flag is read | `--notify-format takes json, slack or telegram, not '<value>'` |
| A `--notify-*` flag without `--notify-url-file` (the first of `--notify-format`, `--notify-telegram-chat-id`, `--notify-token-file`, `--notify-allow-http`) | `<flag> needs --notify-url-file` |
| `--notify-url-file` with `--dry-run` | `--notify-url-file is not used with --dry-run: no explanation is written` |
| `--notify-url-file` without `--endpoint` | `--notify-url-file needs --endpoint` |
| The chat id with `json` or `slack` | `--notify-telegram-chat-id is used only with --notify-format telegram` |
| The token file with `slack` or `telegram` | `--notify-token-file is used only with --notify-format json` |
| `telegram` without a chat id | `--notify-format telegram needs --notify-telegram-chat-id` |
| A chat id that is not an integer or a channel's username | `--notify-telegram-chat-id takes an integer or @ and a channel's username, not '<value>'` |
| The URL file cannot be read | `--notify-url-file '<path>' cannot be read: <the system's error>` |
| The URL file is empty (after trimming) | `--notify-url-file '<path>' is empty` |
| Not a URL | `the URL in --notify-url-file '<path>' is not a valid URL` |
| A scheme other than `https://`, or than `http://` with `--notify-allow-http` | `the URL in --notify-url-file '<path>' must be https:// (or http:// with --notify-allow-http)` |
| A user name or password in the URL | `the URL in --notify-url-file '<path>' must not carry a user name or password` |
| `telegram` and a path other than `/bot<token>/sendMessage` (any host; the query is not checked) | `the URL in --notify-url-file '<path>' must end in /bot<token>/sendMessage with --notify-format telegram` |
| The token file cannot be read, or is empty | `--notify-token-file '<path>' cannot be read: <the system's error>`, `--notify-token-file '<path>' is empty` |
| The HTTP client cannot be built | `cannot build the webhook client: <reason, without the URL>` |

kausa does not check that a listed namespace exists: that would need `get` on `namespaces`, a
cluster-wide permission. With broad credentials, a missing namespace is watched as an empty one
(seen in kind on 2026-10-28; see [Install kausa with Helm](../how-to/install-with-helm.md) for
what the chart does instead); the `started` line shows the list.

`kausa run` connects with the in-cluster service account when it runs in a Pod (when
`KUBERNETES_SERVICE_HOST` is set), and otherwise with the current context of the kubeconfig
(`KUBECONFIG` or `~/.kube/config`). It has no flag to pick a context: run it with a kubeconfig
whose current context is the cluster you mean. It ignores kube-rs's debug variables
(`KUBE_RS_DEBUG_IMPERSONATE_USER`, `KUBE_RS_DEBUG_OVERRIDE_URL` and the others), so the
environment cannot make it impersonate anyone or talk to another server.

## kausa check-endpoint

`kausa check-endpoint` sends one fixed request, built from a fabricated incident, to the model
endpoint, and writes one `endpoint check` line that says which step failed and why
([0020](../adr/0020-check-the-endpoint.md)). It reads nothing from the cluster, sends no second
request, never retries and is outside the call budget. It takes only the flags it shares with
`kausa run`: `--endpoint` and `--model` (both required), `--api-key-file` (or `KAUSA_API_KEY`),
`--allow-http`, `--no-json-schema` and `--model-timeout`; a flag of `kausa run` alone, a flag
given twice or any other argument is a usage error. It exits with `0` when the check passed, `1`
when it failed, and `2` on a usage error or when stdout cannot be written. It never runs on its
own: not at start, not from `kausa run`. The chart runs it as its `helm test` ([Helm
chart](helm-chart.md#the-endpoint-check)).

| Option | What it does |
|---|---|
| `--endpoint <URL>` | Required. The base URL, read and checked as by `kausa run` (`https://`, or `http://` with `--allow-http`) |
| `--model <NAME>` | Required. The model to ask |
| `--api-key-file <PATH>` | A file whose content (trimmed) is the API key; without it, `KAUSA_API_KEY`; without both, no key. Never an argument |
| `--allow-http` | Allow an `http://` endpoint |
| `--no-json-schema` | Do not send `response_format` |
| `--model-timeout <N>` | The call's total timeout in seconds, 10 to 600, `120` by default; connecting has its own 10 seconds |

The usage errors (exit `2`, on stderr as `error: <message>` and a pointer to `kausa --help`),
checked before anything connects, in this order: each argument in turn (a flag of `kausa run`
alone, a flag given twice, a missing value, a bad `--model-timeout`, any other argument), then a
missing `--endpoint`, the key file, the endpoint's scheme and a missing `--model`:

| Case | Message after `error: ` |
|---|---|
| A flag of `kausa run` alone (`--settle-seconds`, `--namespace`, `--no-rollouts`, `--dry-run`, `--no-logs`, `--no-events`, `--history`, `--history-days`, the `--notify-*` flags, `--max-calls-per-hour`, `--max-calls-per-day`, `--max-input-tokens`, `--dedup-hours`, `--metrics-address`, `--leader-lease`) | `<flag> is a flag of kausa run, not of kausa check-endpoint` |
| One of the six flags given twice | `<flag> is given twice` |
| A flag without its value (or with an empty one, but for `--api-key-file`) | `<flag> needs a value` |
| `--model-timeout` outside 10-600, or not a whole number | `--model-timeout takes a whole number of seconds from 10 to 600, not '<value>'` |
| Any other argument | `unexpected argument '<argument>'` |
| No `--endpoint` | `kausa check-endpoint needs --endpoint` |
| The key file cannot be read | `cannot read the API key file '<path>': <the system's error>` |
| `http://` without `--allow-http` | `the endpoint uses plain HTTP: pass --allow-http for an endpoint inside the cluster or the same private network` |
| Another scheme | `the endpoint '<URL>' is not an http(s) URL` |
| No `--model` | `--endpoint needs --model` |

### The request

A fabricated `D001` crash loop compiled into kausa: a Deployment `example-app` in the namespace
`example`, one Pod `example-app-5d9c7b6f4-x2k8p` with a container `app` (image
`registry.example.com/example/app:1.0`) waiting in `CrashLoopBackOff` after exit code `1` and 5
restarts, one `BackOff` Warning event, and a log tail that says it is synthetic. It goes through
the same code as an incident: the context and its redaction, the request with the output mode of
`--no-json-schema` and `max_tokens` 800, the same HTTP client and headers (`Authorization:
Bearer` only with a key), and the validation of the answer. It carries no data from any cluster;
the body is quoted in full in [Data sent to the provider](data-sent-to-the-provider.md#the-endpoint-check)
and pinned by the golden files `tests/golden/check-endpoint-request-json-schema.json` and
`tests/golden/check-endpoint-request-instructions.json` (model `example-model`). One request: no
retry, no wait for `Retry-After`, no call budget and no second request of any kind.

### The `endpoint check` line

One JSON line on stdout, through the same serializer as `kausa run`'s log: `msg` `endpoint
check`, level `info` when the check passed and `error` when it failed, the other fields in
alphabetical order. The answer's text is never printed, nor the API key or the URL's user
information, path or query.

| Field | Value |
|---|---|
| `result` | `ok` or `failed` |
| `cause` | `null` on a pass, a cause of [Causes](#causes), or `invalid_answer` when the chat completion fails kausa's validation |
| `class` | the cause's class (`transient`, `configuration` or `rejected`), or `null` on a pass and for `invalid_answer` |
| `status` | the HTTP status, or `null` when no answer came (`connection`, `timeout`) |
| `connected` | `false` for `connection`, else `true` |
| `authenticated` | `false` for `key`, `true` on a `2xx`, else `null` |
| `model_found` | `false` for `not_found`, `true` on a `2xx`, else `null` |
| `request_accepted` | `false` for `rejected` and `too_large`, `true` on a `2xx`, else `null` |
| `answer_valid` | `true` when the chat completion passed the validation; `false` for `invalid_answer`, `no_content` and `not_an_answer`; else `null` |
| `reason` | kausa's own text, as `kausa run` logs it (`the endpoint answered 404 Not Found`, `the request failed: <reason>`, the fixed text of a `no_content`), `invalid answer: <the validation's error>`, or `null` on a pass |
| `hint` | the fixed hint of the cause (the `invalid_answer` hint below for that cause), or `null` |
| `detail` | the start of a non-`2xx` error body, at most 512 bytes on one line, as `kausa run` logs it; `null` for a `2xx` and when no answer came |
| `retry_after_seconds` | the endpoint's `Retry-After` on a `429` or `503`, clamped to 30-600, or `null` |
| `endpoint` | the scheme, host and port only, the port written even when it is the default (`https://api.example.com:443`) |
| `model`, `json_schema`, `timeout_seconds` | what was checked (`json_schema` is `false` with `--no-json-schema`) |
| `api_key` | `true` when a key was sent, `false` otherwise; never the key |
| `latency_ms` | the call's duration |
| `tokens_in`, `tokens_out` | the `usage` the endpoint reported on a pass, `null` when it reported none or the check failed |
| `prompt_version` | the version of kausa's fixed prompt |

A step is `true` only when the answer proves it, `false` for the step that failed, and `null`
when the answer says nothing either way: any `2xx` proves the key, the model and the request.
Exit status `0` when `result` is `ok`, `1` for every failure: `class` tells a failure worth
retrying later from one that needs a change.

Two lines against Ollama 0.9.2 on the same machine, on 2026-11-02 (`qwen3:0.6b`, then a model
that is not pulled):

```json
{"ts":"2026-11-02T18:18:32.166151Z","level":"info","msg":"endpoint check","answer_valid":true,
 "api_key":false,"authenticated":true,"cause":null,"class":null,"connected":true,"detail":null,
 "endpoint":"http://127.0.0.1:11434","hint":null,"json_schema":true,"latency_ms":1632,
 "model":"qwen3:0.6b","model_found":true,"prompt_version":"2","reason":null,
 "request_accepted":true,"result":"ok","retry_after_seconds":null,"status":200,
 "timeout_seconds":120,"tokens_in":510,"tokens_out":172}
{"ts":"2026-11-02T18:18:36.166081Z","level":"error","msg":"endpoint check","answer_valid":null,
 "api_key":false,"authenticated":null,"cause":"not_found","class":"configuration",
 "connected":true,"detail":"{\"error\":{\"message\":\"model \\\"missing-model\\\" not found, try pulling it first\",\"type\":\"api_error\",\"param\":null,\"code\":null}}",
 "endpoint":"http://127.0.0.1:11434","hint":"the model or the URL is wrong, or the model is no longer offered: check model.name (--model) and that model.endpoint is the base URL",
 "json_schema":true,"latency_ms":0,"model":"missing-model","model_found":false,
 "prompt_version":"2","reason":"the endpoint answered 404 Not Found","request_accepted":null,
 "result":"failed","retry_after_seconds":null,"status":404,"timeout_seconds":120,
 "tokens_in":null,"tokens_out":null}
```

(Wrapped here for reading; kausa writes each line on one line.)

### Causes

Why a model call failed, in `kausa check-endpoint`'s line and in `kausa run`'s log lines
(`cause` in `endpoint unavailable`, `explanation retried` and `endpoint error`): the first row
that matches wins. The class decides what `kausa run` does with the incident and what the call
does to the breaker that holds calls while the endpoint fails ([The breaker](#the-breaker)): a
`transient` failure counts toward opening it, a `configuration` error opens it at once, and the
`rejected` causes, which the request can cause, never move it. The hint is the fixed text the
check prints, and `endpoint unavailable` logs.

| Cause | Matches | Class | Breaker | `kausa run` | Hint |
|---|---|---|---|---|---|
| `connection` | the connection failed: refused, DNS, TLS, or no connection within 10 seconds (hypothesis: reqwest reports all four as connect errors) | `transient` | counts | one retry; the call is given back to the budget | cannot connect: check model.endpoint (--endpoint), DNS, the certificate and networkPolicy.endpoint |
| `timeout` | no answer within `--model-timeout`, a body that stalls, or `408` | `transient` | counts | one retry | no answer within the timeout: the model may be overloaded or slow on this hardware; raise model.timeoutSeconds (--model-timeout) or try later |
| `redirect` | any `3xx` (kausa follows no redirect) | `configuration` | opens | skipped at once (`endpoint`) | the endpoint redirects and kausa follows no redirect: put the final URL in model.endpoint (--endpoint) |
| `not_an_answer` | a `2xx` that is not JSON, JSON with no `choices` array, or over 256 KiB | `configuration` | opens | skipped at once (`endpoint`) | the URL does not serve the OpenAI Chat Completions API: put the base URL (often ending in /v1) in model.endpoint (--endpoint); kausa appends /chat/completions |
| `no_content` | a `2xx` with a `choices` array whose first message has no string `content` (null, absent, another type, or no first element) | `rejected` | none | skipped at once (`endpoint`), with the fixed reason of its kind | the model answered without text: it refused, a filter removed it, or it spent kausa's 800 output tokens before answering (reason says which); with the check's synthetic request, try another model, or one that does not reason at length |
| `rate_limited` | `429` without a quota code | `transient` | counts | one retry, after `Retry-After` | the provider limits the rate of this key: kausa waits and tries again |
| `quota` | `402`, or `429` with the code `insufficient_quota` | `configuration` | opens | skipped at once (`endpoint`) | the provider's account has no quota or credit left: add credit, or use another key (model.apiKey.existingSecret, --api-key-file) |
| `key` | `401`, `403` | `configuration` | opens | skipped at once (`endpoint`) | the endpoint refused the API key: check the Secret in model.apiKey.existingSecret (--api-key-file) and the key's permissions for this model |
| `not_found` | `404`, `405`, `410` | `configuration` | opens | skipped at once (`endpoint`) | the model or the URL is wrong, or the model is no longer offered: check model.name (--model) and that model.endpoint is the base URL |
| `too_large` | `413`, or another `4xx` with the code `context_length_exceeded` | `rejected` | none | skipped at once (`context_too_large`) | the model refuses a request of this size: lower budget.maxInputTokens (--max-input-tokens) or use a model with a larger context |
| `rejected` | any other `4xx` (`400`, `409`, `415`, `422`, …) | `rejected` | none | skipped at once (`endpoint`) | with structured output: the endpoint refused the request: if it does not support structured output, set model.jsonSchema=false (--no-json-schema); see detail. With --no-json-schema: the endpoint refused the request: see detail |
| `server_error` | any `5xx`, any status outside `200`-`499` (`1xx`, `600`), or a body cut short | `transient` | counts | one retry, after `Retry-After` on a `503` | the provider is failing: kausa holds calls and tries again later |

How kausa reads an answer, and nothing more:

- **A non-`2xx` body**, capped at 256 KiB (over it, the status alone decides), is parsed as JSON
  (an array by its first element), and only `error.code` and `error.type` are read, each only when
  it is a string of at most 64 bytes, and compared byte for byte with `insufficient_quota` (on a
  `429`) and `context_length_exceeded` (on another `4xx`). The first was answered by a hosted
  provider on 2026-10-30; the second is a hypothesis (a wrong entry only changes the skip reason,
  since both rejected causes are neutral). Only a cause and a clamped delay come out of the body;
  its start stays in `detail`, in kausa's log and the check's line only.
- **A `2xx`**: a string `choices[0].message.content`, even empty, is the answer, which the
  validation judges. Otherwise, with a `choices` array, it is `no_content`, and its kind comes
  from fixed keys only: `choices[0].message.refusal` present and not null is `the model answered
  with no content: it refused`; `choices[0].finish_reason` equal to `content_filter` is `the model
  answered with no content: a content filter removed it`, to `length` `the model answered with
  no content: it ran out of output tokens`; anything else is `the model answered with no
  content`. These are the shapes a structured-output refusal, an output filter and a reasoning
  model that spends `max_tokens` before answering may take (hypotheses: no such body has been
  captured). The refusal's text and the `finish_reason`'s value are never logged.
- **`Retry-After`** is read on `429` and `503` only, as 1 to 10 ASCII digits (no sign, space or
  date), clamped to 30-600 seconds (`0` is 30); anything else is ignored. It sets that incident's
  retry delay, and the shortest hold when that answer opens the breaker.

A completion that fails validation is `invalid_answer`, with no class: the endpoint works, so the
breaker counts it as an answer. Its hint: the model answered, but not with kausa's JSON: try
another model, or model.jsonSchema=false (--no-json-schema) if the server ignores
response_format.

## The breaker

`kausa run` holds its model calls while the endpoint fails, in front of the call budget
([0020](../adr/0020-check-the-endpoint.md), [0021](../adr/0021-refill-the-call-budget.md)). Its
constants are not flags:

- **Closed** (the start): calls go out within the budget. 3 transient failures in a row, or 1
  configuration error, **open** it for `max(60 s, Retry-After)` and log `endpoint unavailable`.
  An answer resets the count.
- **Open**: no call, no budget taken and no log read. A queued incident whose explanation is
  still in the deduplication cache is served from it; when 16 are queued, the oldest is skipped
  (reason `endpoint_unavailable`, an `ExplanationSkipped` Event whose note is `kausa could not
  explain this failure: the model endpoint is unavailable; this incident waited and was not
  explained`); the others wait (`explanations waiting`, reason `the model endpoint is
  unavailable`).
- **Half open**, once the hold ends: the next incident admitted, within the budget, is the probe.
  A failed probe opens it again with the hold doubled (60, 120, 240, 480, 960, then 1800 seconds
  at most, or `Retry-After` when longer) and logs `endpoint unavailable` again; any chat
  completion, valid or not, closes it, logs `endpoint available` and resets the count and the
  hold. A `rejected`, `too_large` or `no_content` answer changes nothing in any state.
- A transient first failure that leaves the breaker open sends its incident to the **back** of the
  queue for a second and last call, so successive probes are different incidents; while the
  breaker stays closed, it is tried again after 30 seconds or `Retry-After`. An incident is called
  at most twice per process.
- The breaker lives in memory, one per process, and is kept across terms with `--leader-lease`; a
  restart resets it. A passing `kausa check-endpoint` does not close it: the check is another
  process.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | `--help` or `--version`, a clean stop after `SIGTERM` or Ctrl-C, or a `kausa check-endpoint` that passed |
| `1` | A `kausa check-endpoint` that failed, whatever the cause; or kausa cannot start (no client configuration, the API server is unreachable, it cannot listen on `--metrics-address`) or a watch ended: `the pod watch ended`, `the event watch ended` or `the deployment watch ended` for the whole cluster, `the pod watch of namespace <name> ended`, `the event watch of namespace <name> ended` or `the deployment watch of namespace <name> ended` with `--namespace`, in the `stopped on an error` line. Watch errors do not end a watch: they are logged and retried |
| `2` | Usage error (unknown argument, invalid flag value such as a `--metrics-address` that is not an address and port, an `http://` endpoint without `--allow-http`, an unreadable API key file, a notification flag or URL kausa refuses, a `--model-timeout` outside 10-600, a flag of `kausa run` given to `kausa check-endpoint`), or stdout cannot be written (also while running: `kausa run` stops when it cannot write its log, and says so on stderr) |

## Log lines

`kausa run` writes one JSON object per line on standard output, through a serializer, so a value
taken from the cluster cannot break a line or add a field. Every line starts with `ts` (RFC 3339,
UTC), `level` (`info`, `warn` or `error`) and `msg`; the other fields follow in alphabetical
order. Messages and notes taken from the cluster are cut at 1024 bytes.

| `msg` | Level | Other fields |
|---|---|---|
| `started` | info | `version`, `server` (the API server's version), `settle_seconds`, `namespaces` (the watched namespaces, sorted, or `null` for the whole cluster), `rollouts` (`true` while Deployments are watched, `false` with `--no-rollouts`), `notifications` (`{"format", "host"}`, the webhook's host only, with no port, path or query, or `null` when they are off), `leader_lease` (`"<namespace>/<name>"`, the flag's value, or `null` without `--leader-lease`), `model_timeout_seconds` (`--model-timeout`, or `null` without a model client: no `--endpoint`, or `--dry-run`) |
| `incident` | info | See below |
| `pod watch error`, `event watch error`, `deployment watch error` | warn | `namespace` (the namespace of the failing watch, or `null` for the cluster-wide one), `error` (kube-rs's text), and `folded` (how many repeats of that error were not logged since its previous line), only when some were. The watch retries with backoff, from 0.8 up to 30 seconds. A watch's (kind and namespace) first error is logged, and so is one whose text differs from that watch's last line; the same text again is logged at most once every 5 minutes, even if the watch worked in between. Every error is counted in `kausa_watch_errors_total`, including repeats folded after an error's last line, which no line reports. A refusal (`403`) drops the records that watch brought in, until it lists again |
| `owner lookup failed` | warn | `namespace`, `pod`, `job` or `replicaset` (the object a Pod or an event is about), `error`; until a retry works (a failed lookup is retried after 30 seconds, on the Pod's or the event's next update), the incident names the ReplicaSet or Job instead |
| `stopped` | info | |
| `request` | info | With `--dry-run`: `detection`, `namespace`, `owner`, `fingerprint`, `request` (the exact body that would be sent) and `redactions` (replacements per rule) |
| `explanation` | info | `detection`, `namespace`, `owner`, `fingerprint`, `explanation` (`summary`, `cause`, `steps`, `confidence`, with the real names mapped back), `prompt_version`, `tokens_in`, `tokens_out` (as the endpoint reported them), `redactions` (the request's replacements per rule) and `answer_redactions` (the values replaced in the model's answer per rule, `{}` when none; [On the model's answer](rules.md#on-the-models-answer)); or, when it comes from the deduplication cache, `explanation` and `"cached": true`, with neither count. The `explanation` is the redacted answer: kausa keeps no unredacted copy |
| `explanation retried` | warn | `detection`, `namespace`, `owner`, `fingerprint`, `reason`, `detail` (the start of the endpoint's error body, at most 512 bytes on one line, or `null`), `cause` (a `transient` cause of [Causes](#causes)), `queued` and `retry_in_seconds`: the first call failed with a transient cause and the incident gets one more. With `queued` `false` the breaker is still closed, and the call is tried again after `retry_in_seconds`, 30 or the endpoint's `Retry-After` (30 to 600); with `queued` `true` the failure left the breaker open, the incident went to the back of the queue, and `retry_in_seconds` is the time left in the hold, rounded up. A call that never reached the endpoint (a failed connection) is given back to the budget |
| `endpoint error` | warn | `detection`, `namespace`, `owner`, `fingerprint`, `reason`, `detail`, `cause`, `status` (the HTTP status, or `null`): the endpoint's error body, logged before the `explanation skipped` line it leads to, when there is one. The body is the provider's text, so it goes to the log only, not to the Event |
| `endpoint unavailable` | warn | `cause`, `class`, `status`, `reason`, `hint`, `detail` (as in [Causes](#causes)), `failed_calls` (the failed calls since the failures began: 3 after transient ones, 1 after a configuration error, one more per failed probe), `probe_in_seconds` (the hold): the breaker opened, on the call this line describes ([The breaker](#the-breaker)). Logged at each move into open, so once per hold; no incident field |
| `endpoint available` | info | `unavailable_seconds` (since the breaker first opened), `failed_calls`: a chat completion closed the breaker. No incident field |
| `explanation skipped` | warn | `detection`, `namespace`, `owner`, `fingerprint`, `reason`: the call budget is spent, the context does not fit `--max-input-tokens` or the endpoint refused it as too large (`the endpoint refused the request as too large for the model: lower --max-input-tokens`), the endpoint failed or answered an error status (a configuration error at once, a transient one after its second call), the model answered with no content (the fixed reason of its kind), the answer did not validate, or the model endpoint is unavailable. A skip for the budget or the breaker comes before the log is read, so it reads no log: the daily cap is spent, a cap is `0`, or the incident was the oldest of a full queue of 16 waiting for the budget or for the breaker (`the model endpoint is unavailable; this incident waited and was not explained`) |
| `explanations waiting` | info | `reason` (`the hourly call budget is spent`, or `the model endpoint is unavailable` while the breaker is open), `queued` (the incidents left in the queue), `next_call_in_seconds` (when the next call may start, rounded up: for the breaker, the end of the hold): incidents wait for the refill bucket, the 60-minute cap or the breaker. Logged once per waiting period, the first time the slot is held since the queue was last empty; not per incident or per pass |
| `event write failed` | warn | `fingerprint`, `error`: the Event could not be created or patched; `no answer within 10 seconds` when the whole write (create, and on a repeat its list and series patch) took longer |
| `explanation dropped` | warn | `fingerprint`, `reason` (`the queue is full`), `waiting_for_budget` (`true` when the budget made the slot wait the last time it was asked; `false` while the breaker holds it): the queue of 16 incidents waiting for an explanation was full when a new incident or a delayed retry joined it, and the oldest was dropped, with no Event |
| `history write failed` | warn | `fingerprint`, `namespace`, `error`: the Explanation could not be written. `error` is `the CustomResourceDefinition explanations.kausa.fedeam.com is not installed (chart value history.installCRD)` when the list answered `404`, `no answer within 10 seconds` on the timeout, `409 Conflict: the Explanation was created or changed by another writer since it was read; not retried`, the code and reason only (for example `422 Invalid`) when the API server refused the list, create or patch, since its message can quote the stored object, and kube-rs's text otherwise |
| `history write refused` | warn | `fingerprint`, `namespace`, `reason`: an object of that name exists and is not kausa's to append to: `the Explanation kausa.<fingerprint> does not carry the label app.kubernetes.io/managed-by=kausa` or `the Explanation kausa.<fingerprint> is about another rule or target` |
| `history sweep failed` | warn | `namespace` (the listed namespace whose list or delete failed, or `null` in cluster scope), `error` (the same `404` text when the CRD is missing; otherwise, when the API server refused the list or a delete, its code and reason only, since its message can quote an object or name a listed namespace, and kube-rs's text for any other failure). A failed list ends that namespace's part of the sweep; the next namespace goes on |
| `history swept` | info | `deleted_by_age`, `deleted_by_cap`: what one sweep deleted, only when it deleted something |
| `log read failed` | warn | `namespace`, `pod`, `container`, `error`; the request is built without the log |
| `notification sent` | info | `fingerprint`, `namespace`, `owner` (`kind`, `name`), `format`, `status` (the HTTP status code, a number) and `attempt` (`1`, or `2` after a retry): the webhook answered `2xx` |
| `notification retried` | warn | `fingerprint`, `reason`, `retry_in_seconds` (`30`): the first attempt timed out, could not connect, or was answered `429` or a `5xx` status, and is tried once more |
| `notification failed` | warn | `fingerprint`, `reason`: `the webhook answered <status>` (any status but `2xx`, `3xx`, `429` and `5xx`, or one of those two after the retry), `a redirect is not followed: put the final URL in the file` for a `3xx`, or `the request failed: <reason>` without the URL. Final: the notification is queued again only if the incident comes back |
| `notification dropped` | warn | `fingerprint`, `reason` (`the queue is full`): the queue of 16 notifications was full, and the oldest was dropped |
| `stopped on an error`, `cannot start the runtime` | error | `error` |
| `standby` | info | `lease`: with `--leader-lease`, this replica waits; after its first successful read of the Lease, and after each step-down |
| `leading` | info | `lease`: this replica took the Lease and starts its watches |
| `leadership lost` | warn | `lease`, `reason`: `not renewed in time` (no renewal succeeded within 20 seconds of the last good one) or `held by another` (the Lease names another replica). The replica drops its watches, records, queues and owner cache, keeps its call budget and deduplication cache, and waits as a standby; the process does not exit |
| `lease released` | info | `lease`: on a stop (`SIGTERM`, Ctrl-C) or a watch that ended, the leader cleared the holder, so the standby takes over at its next read |
| `lease release failed` | warn | `lease`, `error`: the release did not succeed within 2 seconds; the standby takes over 30 seconds after the last renewal instead |
| `lease error` | warn | `lease`, `call` (`get`, `patch`), `error` (`404 NotFound`, `no answer within the call's timeout`, the code and reason of the API server's answer without its message, or kube-rs's text), and `hint`: `kausa needs get and patch on this Lease (docs/reference/permissions.md)` on a `403`, `the chart creates this Lease: run helm upgrade` on a `404`, absent otherwise. Logged when an error starts, when its text changes, and at most once a minute while it lasts; every failed call is counted. A `409` (the Lease changed since it was read) is not an error |

An `incident` line, with invented values:

```json
{"ts":"2026-10-26T12:00:00Z","level":"info","msg":"incident","detection":"D001",
 "detection_name":"crash loop","fingerprint":"5d0c2b7e9a1f4c38",
 "first_seen":"2026-10-26T11:59:00Z","namespace":"shop",
 "owner":{"apiVersion":"apps/v1","kind":"Deployment","name":"web"},
 "pods":["web-7d9c8b6f4d-2xkqp","web-7d9c8b6f4d-8hn2w"],"pods_total":2,
 "sample":{"container":"web","exit_code":1,
           "message":"back-off 1m20s restarting failed container=web","pod":"web-7d9c8b6f4d-2xkqp",
           "reason":"CrashLoopBackOff"}}
```

A `started` line with `--namespace shop --namespace payments`, and a watch error of one of those
namespaces that lasts, with invented values: the second line comes 5 minutes after the first, and
folds the 9 repeats in between.

```json
{"ts":"2026-10-28T12:00:00Z","level":"info","msg":"started","namespaces":["payments","shop"],
 "rollouts":true,"server":"v1.36.4","settle_seconds":60,"version":"0.4.0"}
{"ts":"2026-10-28T12:00:01Z","level":"warn","msg":"pod watch error","error":"<kube-rs's text>",
 "namespace":"payments"}
{"ts":"2026-10-28T12:05:01Z","level":"warn","msg":"pod watch error","error":"<kube-rs's text>",
 "folded":9,"namespace":"payments"}
```

An `incident` line of a stalled rollout (`D014`), with invented values: it is about the
Deployment itself and lists no Pods; `sample.message` is `revision <n>`, the Deployment's revision
annotation when it is an unsigned integer (empty otherwise), and is never sent to the model.

```json
{"ts":"2026-10-29T12:00:00Z","level":"info","msg":"incident","detection":"D014",
 "detection_name":"rollout does not progress","fingerprint":"0b3f6a9c2d7e1f45",
 "first_seen":"2026-10-29T11:59:00Z","namespace":"shop",
 "owner":{"apiVersion":"apps/v1","kind":"Deployment","name":"web"},
 "pods":[],"pods_total":3,
 "sample":{"container":null,"exit_code":null,"message":"revision 7","pod":null,
           "reason":"ProgressDeadlineExceeded"}}
```

(Wrapped here for reading; kausa writes each line on one line.)

- `detection`, `detection_name`: the rule, from [Detection and redaction rules](rules.md).
- `owner`: the top-level owner the Pods were correlated to.
- `pods`: the Pods whose failures make up the incident, sorted; empty for a failed Job. Past 100
  Pods seen while the failure was pending, the first 100 names in sorted order and the Pods failing
  when it settled ([Detection and redaction rules](rules.md)).
  `pods_total`: the Pods kausa currently knows with that owner.
- `fingerprint`: 16 hexadecimal digits that stay the same for the same failure of the same
  workload, whichever Pod shows it and however long the back-off has grown ([Detection and
  redaction rules](rules.md)).
- `sample`: the first failing container or event, in a fixed order.

With `--leader-lease`, `lease` is always the flag's own value: no log line carries the Lease's
holder or any other value read from it.

No log line carries the webhook's URL or token, the body of a notification or the webhook's
answer: kausa reads only its status code.

Nothing read back from a stored Explanation appears in a log line: the history lines carry only
kausa's own fingerprint and namespace, fixed texts and the API server's error codes.

The log carries real names and messages from the cluster, as [Data sent to the
provider](data-sent-to-the-provider.md) says under "What stays in the cluster": who can read
kausa's log sees them.

## Metrics

With `--metrics-address`, `GET /metrics` returns these counters and the gauges below in the
Prometheus text format; each counter is `0` until something counts. `GET /healthz` answers
`200` while kausa runs, and `GET /readyz` answers `200` with `ready` once every watch has listed at
least once (the three of the whole cluster, or three per namespace with `--namespace`: 3 × N; two
and 2 × N with `--no-rollouts`), and `503` with `starting` before. With `--leader-lease`, a
replica is also Ready after its first successful read of the Lease leaves it a standby, and then
answers `200` with `standby` for as long as it does not hold the Lease; a replica that takes the
Lease at its first read is Ready only once its watches have listed. It never goes back to `503`,
not even while a promoted standby lists; a watch refused before its first list keeps kausa not
ready. No path,
label or body names a namespace. Any other path answers `404`,
and any other method `405`. The counters live in memory and start over when kausa restarts.

| Counter | Labels | Counts |
|---|---|---|
| `kausa_incidents_total` | `detection` | Incidents that settled (`D014` included; a `D014` handled by another incident is not counted) |
| `kausa_model_calls_total` | `outcome` (`ok`, `error`, `invalid`) | Calls to the endpoint |
| `kausa_model_tokens_total` | `direction` (`in`, `out`) | Tokens the endpoint reported |
| `kausa_explanations_cached_total` | | Incidents explained again from the deduplication cache |
| `kausa_explanations_skipped_total` | `reason` (`budget`, `context_too_large`, `endpoint`, `invalid_answer`, `endpoint_unavailable`) | Incidents not explained; `endpoint_unavailable`: the oldest of a full queue of 16 while the breaker was open |
| `kausa_explanations_retried_total` | | Incidents given a second call after a transient error, tried again after a delay or sent to the back of the queue |
| `kausa_explanations_dropped_total` | | Incidents dropped because the queue of 16 was full |
| `kausa_requests_logged_total` | | Requests logged instead of sent (`--dry-run`) |
| `kausa_redactions_total` | `rule` | Values replaced by redaction in the requests (help text: `Values replaced by redaction in the requests, by rule.`) |
| `kausa_answer_redactions_total` | `rule` (the twelve ids of [On the model's answer](rules.md#on-the-models-answer), never `R002`) | Values replaced by redaction in the model's answers (help text: `Values replaced by redaction in the model's answers, by rule.`). See below what a rise means |
| `kausa_events_written_total` | `reason` (`Explained`, `ExplanationSkipped`) | Events created or updated |
| `kausa_event_write_errors_total` | | Events that could not be written |
| `kausa_history_writes_total` | `outcome` (`created`, `updated`, `refused`, `conflict`, `error`) | Explanation objects written, by outcome |
| `kausa_history_deleted_total` | `reason` (`age`, `cap`) | Explanation objects deleted by the sweep, by reason |
| `kausa_history_sweep_errors_total` | | Failed list or delete calls of the history sweep (a delete answered `404` or `409` is not one) |
| `kausa_notifications_total` | `outcome` (`sent`, `failed`, `dropped`) | Notifications by final outcome ([Notifications](#notifications)) |
| `kausa_notifications_retried_total` | | Notifications tried again after a transient error |
| `kausa_leadership_changes_total` | `to` (`leader`, `standby`) | Times this process took the Lease or stepped down to standby (help text: ``Times this process became leader or stepped down to standby, by `to` (leader, standby).``) |
| `kausa_lease_errors_total` | `call` (`get`, `patch`) | Failed calls on the Lease; a `409` is not one (help text: ``Failed calls on the Lease, by `call` (get, patch); conflicts are not errors.``) |
| `kausa_watch_errors_total` | `watch` (`pods`, `events`, `deployments`) | Errors of the Pod, Event and Deployment watches (every namespace's together; the namespace is in the log line), which retry with backoff (the counter's help text: `Errors of the Pod, Event and Deployment watches, which retry with backoff, by watch.`); a steady rise with no incidents means kausa is not seeing the cluster |

The gauges, rendered after the counters, in this order, each with `# TYPE <name> gauge`.
`kausa_leader` is always rendered, and `kausa_endpoint_state` from the start of `kausa run` in
every mode; the others only once they have a value, so a mode that does not set them shows no line
for them, not a `0`:

| Gauge | Labels | Value |
|---|---|---|
| `kausa_leader` | | `1` while this process acts: it holds the Lease, or runs without `--leader-lease`; `0` as a standby (help text: `1 while this process acts (it holds the Lease, or runs without --leader-lease), 0 as a standby.`). Summed over a release's Pods, anything but `1` means nobody acts, or (never expected) two do |
| `kausa_budget_calls_remaining` | `window` (`hour`, `day`) | With `--endpoint` only. `hour`: the whole calls in the refill bucket, within the 60-minute cap (`--max-calls-per-hour` minus the calls of the last 60 minutes): the calls this process may start now. On a standby, at most 3: what a takeover would bring. `day`: `--max-calls-per-day` minus the calls of the last 24 hours. Refreshed when a metrics connection is accepted, on the leader and on a standby alike. `0` for `hour` is normal while incidents wait; `0` for `day` on the leader means no call until the oldest of the day's calls is 24 hours old (help text: `Model calls this process may start now, by window (hour: the refill bucket within the 60-minute cap; day: the 24-hour cap).`) |
| `kausa_explanation_queue_length` | | With `--endpoint` or `--dry-run`. Incidents waiting for an explanation, 0 to 16: not the one being explained, nor the delayed retries. Set once per pass of the loop; `0` on a standby and after a step-down (help text: `Incidents waiting for an explanation (at most 16).`) |
| `kausa_endpoint_state` | `state` (`closed`, `half_open`, `open`) | Three series under one `# HELP` and `# TYPE`, exactly one at `1`: the state of [the breaker](#the-breaker). Set to `closed` at start in every mode (also without `--endpoint` and with `--dry-run`), then from the breaker at each pass of the loop while this process acts, and to `closed` on a standby and after a step-down (help text: `State of the model endpoint's breaker: 1 for the current state (closed, open, half_open), 0 for the others; closed on a standby.`) |
| `kausa_model_last_success_timestamp_seconds` | | With `--endpoint` and without `--dry-run` only. The Unix time of the last call answered with a chat completion, valid or not; `0` before the first. Kept across terms (help text: `Unix time of the last call answered with a chat completion, 0 before the first.`) |

No metric names a Pod, a namespace or the Lease.

A rise of `kausa_answer_redactions_total` means an answer carried a value kausa would not send:
usually text injected into the failing workload's logs or events, a value the model invented, or
a host it built from placeholders (`deployment-1.namespace-1.svc.cluster.local` becomes `host-N`).
The answer was written redacted all the same, and kausa keeps no unredacted copy. To see which
incident it was, read the `answer_redactions` field of the `explanation` log lines, then that
workload's logs and events. Some benign answers count too, such as `127.0.0.1` (`R004`) or a link
to a public site outside `R013`'s allowlist (`R013`).

## Leader election

With `--leader-lease <NAMESPACE>/<NAME>`, `kausa run` connects, logs `started` and serves its
metrics as usual, then reads the Lease every 5 seconds (`RETRY_PERIOD`) and starts nothing else:
no watch, owner lookup, model call or write. It takes the Lease when it has no holder (empty or
absent), names this replica, or has not changed (same `resourceVersion`) for 30 seconds
(`LEASE_DURATION`) on this replica's own monotonic clock. Then it logs `leading`, runs the
controller as without the flag, and renews the Lease every 5 seconds. It acts until 20 seconds
(`RENEW_DEADLINE`) after it sent its last good renewal: past that, or as soon as the Lease names
another replica, it logs `leadership lost`, drops everything it was doing and waits again as a
standby. Every write to the cluster and every webhook attempt checks that the term still holds
before it starts, and each ends within 10 seconds (`WRITE_BOUND`), so a deposed leader is done
before a standby may take over. These are constants, not flags.

The calls are a `get` and a JSON merge patch of the Lease's `spec` that carries the
`resourceVersion` kausa last read, each within 5 seconds; a `409` after a renewal is read once,
and the renewal is sent again with the new version while the Lease still names this replica (a
label change by Helm moves the version). On a stop or a watch that ended, the leader releases the
Lease (holder `""`, `leaseDurationSeconds: 1`) within 2 seconds, then exits as without the flag.
kausa writes `holderIdentity`, `leaseDurationSeconds`, `acquireTime`, `renewTime` and
`leaseTransitions` for readers such as `kubectl`, and reads only the holder and the version:
neither clock skew nor an edited time or duration moves an expiry. A lost Lease never ends the
process and never changes the exit code.

## Notifications

With `--notify-url-file`, each new explanation is queued after its Event (or after it would be
written, with `--no-events`) and sent by its own task, one at a time, at least 1 second apart (3
seconds with `telegram`), from a queue of 16 that drops its oldest. A cached repeat is queued again
only while the webhook has not acknowledged that explanation with a `2xx`. A send never delays an
explanation or an Event, never stops kausa and never changes its exit code; queued notifications
are dropped at `SIGTERM` and, with `--leader-lease`, when the leader steps down (counted as
`dropped`). What is sent: [Data sent in
notifications](data-sent-in-notifications.md).

A `started` line with notifications in the `json` format, with invented values:

```json
{"ts":"2026-10-29T12:00:00Z","level":"info","msg":"started","namespaces":null,
 "notifications":{"format":"json","host":"hooks.example.com"},"server":"v1.36.4",
 "settle_seconds":60,"version":"0.2.0"}
```

Alert on failed notifications, for example:

```text
increase(kausa_notifications_total{outcome="failed"}[1h]) > 0
```

How to set it up and check it: [Send notifications to Slack, Telegram or a
webhook](../how-to/send-notifications.md).
