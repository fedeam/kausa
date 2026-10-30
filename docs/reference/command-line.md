# Command line and log format

The commands and flags of the `kausa` binary, its exit codes, and the JSON lines it writes.

Status as of 2026-10-29: what v0.3.0 does, with the namespaced mode, the
explanation history and the notifications it added (`--namespace`, `--history`,
`--history-days` and the `--notify-*` flags). `kausa
run` detects and groups failures and logs incidents; with `--endpoint` it explains each one within
a call budget, reuses explanations of repeated incidents, and writes the explanation as an Event
on the failing workload; with `--dry-run` it logs the request instead; with `--metrics-address` it
serves Prometheus metrics ([0001. Build a small, read-only explainer that writes one redacted
explanation per incident](../adr/0001-scope-and-positioning.md)); with `--namespace` it watches
only the namespaces given ([0013. Offer a namespaced install mode: one watch pair and one Role per
listed namespace, and nothing cluster-wide](../adr/0013-namespaced-install-mode.md)); with
`--history` it also keeps each explained incident's full explanations in an Explanation object in
its namespace ([0014. Keep each incident's full explanations in a namespaced Explanation resource,
off by default](../adr/0014-explanation-history.md)); with `--notify-url-file` it also posts each
new explanation to one webhook ([0015. Post each new explanation to one optional webhook, as JSON,
Slack or Telegram](../adr/0015-notifications.md)). The work for v0.4 adds a watch on Deployments
for stalled rollouts (`D014`), on unless `--no-rollouts` ([0018. Explain a Deployment past its
progress deadline (`D014`) from a Deployment watch that is on by default, and add nothing for
StatefulSets or DaemonSets](../adr/0018-stuck-rollouts.md)), and `--leader-lease`, which makes a
replica act only while it holds a Lease, for a standby beside it ([0019. Offer an optional standby
replica that takes over through a chart-created Lease, elected by kausa's own
code](../adr/0019-high-availability.md)).

## Commands

| Command | What it does |
|---|---|
| `kausa run [OPTIONS]` | Runs the controller until `SIGTERM` or Ctrl-C |
| `kausa -h`, `kausa --help` | Prints the help |
| `kausa -V`, `kausa --version` | Prints `kausa <version>` |

Options of `kausa run`:

| Option | What it does |
|---|---|
| `--settle-seconds <N>` | How long a failure has to persist before it is an incident, from `0` to `3600`; `60` by default. A failed Job (`D008`) is an incident at once, whatever the value |
| `--namespace <NAME>` | Watch only this namespace: one Pod watch, one Warning event watch and, unless `--no-rollouts`, one Deployment watch in it. Repeat it for more, one name each time, up to 32; the order does not matter. Without it, kausa watches the whole cluster (one watch of each kind across all namespaces). The scope comes only from this flag, never from the kubeconfig's context or the service account's namespace |
| `--no-rollouts` | Do not watch Deployments, so a rollout past its progress deadline (`D014`) is not explained. The watch is on by default and needs `list` and `watch` on `deployments`; turn it off when kausa's role does not grant them. Independent of `--endpoint` and `--dry-run` |
| `--endpoint <URL>` | The base URL of an OpenAI-compatible endpoint, such as `https://api.example.com/v1` or `http://127.0.0.1:11434/v1` for Ollama. Each incident's redacted context is sent to `<URL>/chat/completions` and the answer is logged as an explanation. Needs `--model` |
| `--model <NAME>` | The model to ask |
| `--api-key-file <PATH>` | A file whose content (trimmed) is the API key, sent as `Authorization: Bearer`. Without it, the environment variable `KAUSA_API_KEY`; without both, no key. Never an argument |
| `--allow-http` | Allow an `http://` endpoint, for one on the same machine, in the cluster or in the same private network. With `--dry-run` the endpoint is not used, so its scheme is not checked |
| `--dry-run` | Build each request and log it instead of sending it; no endpoint needed |
| `--no-logs` | Do not read container logs (the request has no `log_tail`) |
| `--no-json-schema` | Do not send `response_format`, for servers without structured output; the answer is validated all the same |
| `--no-events` | Do not write Events; the explanations are only logged. Events are never written in dry-run |
| `--history` | Keep each explained incident's explanations (fresh or from the deduplication cache) in an Explanation object named `kausa.<fingerprint>` in its namespace, after the Event and independent of it, and delete old ones: needs the CustomResourceDefinition `explanations.kausa.fedeam.com` ([Explanation resource](explanation-resource.md)). Needs `--endpoint`; not with `--dry-run`. With `--no-events`, kausa writes the history only |
| `--history-days <N>` | Delete Explanations, and drop entries, last written more than `N` days ago, from `1` to `30`; `7` by default. Needs `--history` |
| `--notify-url-file <PATH>` | Post each new explanation, after its Event, to the webhook whose URL is the content of this file (trimmed), read once at start. The URL is a secret: no flag value or environment variable form, and it is never logged. Needs `--endpoint`; not with `--dry-run`. What is sent: [Data sent in notifications](data-sent-in-notifications.md) |
| `--notify-format <FORMAT>` | The body: `json` (the default), `slack` or `telegram` |
| `--notify-telegram-chat-id <ID>` | With `telegram` only, and required with it: the chat, an integer that is not `0` (a leading `-`, no `+`) or `@` and a channel's username (a letter, then 4 to 31 letters, digits or `_`). Not a secret |
| `--notify-token-file <PATH>` | With `json` only, optional: a file whose content (trimmed) is sent as `Authorization: Bearer <token>` |
| `--notify-allow-http` | Allow an `http://` webhook, for one in the cluster or the same private network; separate from `--allow-http` |
| `--max-calls-per-hour <N>` | Model calls allowed in any hour, `30` by default; `0` allows none |
| `--max-calls-per-day <N>` | Model calls allowed in any 24 hours, `200` by default |
| `--max-input-tokens <N>` | Cap on a request's estimated input tokens (a quarter of its bytes), `6000` by default: the context is cut to fit, and an incident that still does not fit is skipped |
| `--dedup-hours <N>` | How long an incident's explanation is reused when the same incident (same fingerprint) comes back, `6` by default; `0` turns the reuse off |
| `--metrics-address <ADDR>` | Serve `/metrics`, `/healthz` and `/readyz` on this IP address and port, for example `[::]:9090` in a Pod (IPv4 and IPv6 on Linux) or `127.0.0.1:9090` on a machine; not served by default |
| `--leader-lease <NAMESPACE>/<NAME>` | Act only while holding this Lease, and wait as a standby otherwise: for two replicas, of which one acts ([Leader election](#leader-election)). The Lease must exist (the Helm chart creates it with `replicaCount: 2`); kausa needs `get` and `patch` on it and never creates it. Needs `POD_NAME`, the identity this replica holds the Lease with. Works in every mode, log-only and `--dry-run` included |

Without `--endpoint` or `--dry-run`, kausa only logs incidents. The Events are written as
`kausa` with the reporting instance from the environment variable `POD_NAME` (set it from the
Pod's name with the downward API, as the Helm chart does), or `kausa` (only without
`--leader-lease`, which requires `POD_NAME`; the holder of the Lease and the reporting instance of
its Events are then the same name). `--model`, `--api-key-file`, `--allow-http` and
`--no-json-schema` alone are usage errors.

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

## Exit codes

| Code | Meaning |
|---|---|
| `0` | `--help` or `--version`, or a clean stop after `SIGTERM` or Ctrl-C |
| `1` | kausa cannot start (no client configuration, the API server is unreachable, it cannot listen on `--metrics-address`) or a watch ended: `the pod watch ended`, `the event watch ended` or `the deployment watch ended` for the whole cluster, `the pod watch of namespace <name> ended`, `the event watch of namespace <name> ended` or `the deployment watch of namespace <name> ended` with `--namespace`, in the `stopped on an error` line. Watch errors do not end a watch: they are logged and retried |
| `2` | Usage error (unknown argument, invalid flag value such as a `--metrics-address` that is not an address and port, an `http://` endpoint without `--allow-http`, an unreadable API key file, a notification flag or URL kausa refuses), or stdout cannot be written (also while running: `kausa run` stops when it cannot write its log, and says so on stderr) |

## Log lines

`kausa run` writes one JSON object per line on standard output, through a serializer, so a value
taken from the cluster cannot break a line or add a field. Every line starts with `ts` (RFC 3339,
UTC), `level` (`info`, `warn` or `error`) and `msg`; the other fields follow in alphabetical
order. Messages and notes taken from the cluster are cut at 1024 bytes.

| `msg` | Level | Other fields |
|---|---|---|
| `started` | info | `version`, `server` (the API server's version), `settle_seconds`, `namespaces` (the watched namespaces, sorted, or `null` for the whole cluster), `rollouts` (`true` while Deployments are watched, `false` with `--no-rollouts`), `notifications` (`{"format", "host"}`, the webhook's host only, with no port, path or query, or `null` when they are off), `leader_lease` (`"<namespace>/<name>"`, the flag's value, or `null` without `--leader-lease`) |
| `incident` | info | See below |
| `pod watch error`, `event watch error`, `deployment watch error` | warn | `namespace` (the namespace of the failing watch, or `null` for the cluster-wide one), `error` (kube-rs's text); the watch retries with backoff, from 0.8 up to 30 seconds. A refusal (`403`) drops the records that watch brought in, until it lists again |
| `owner lookup failed` | warn | `namespace`, `pod`, `job` or `replicaset` (the object a Pod or an event is about), `error`; until a retry works (a failed lookup is retried after 30 seconds, on the Pod's or the event's next update), the incident names the ReplicaSet or Job instead |
| `stopped` | info | |
| `request` | info | With `--dry-run`: `detection`, `namespace`, `owner`, `fingerprint`, `request` (the exact body that would be sent) and `redactions` (replacements per rule) |
| `explanation` | info | `detection`, `namespace`, `owner`, `fingerprint`, `explanation` (`summary`, `cause`, `steps`, `confidence`, with the real names mapped back), `prompt_version`, `tokens_in`, `tokens_out` (as the endpoint reported them) and `redactions`; or, when it comes from the deduplication cache, `explanation` and `"cached": true` |
| `explanation retried` | warn | `detection`, `namespace`, `owner`, `fingerprint`, `reason`, `detail` (the start of the endpoint's error body, at most 512 bytes on one line, or `null`), `retry_in_seconds`: the first call failed with a timeout, a failed connection, `429` or a `5xx` status, and is tried once more 30 seconds later. A call that never reached the endpoint (a failed connection) is given back to the budget |
| `endpoint error` | warn | `detection`, `namespace`, `owner`, `fingerprint`, `reason`, `detail`: the endpoint's error body, logged before the `explanation skipped` line it leads to. The body is the provider's text, so it goes to the log only, not to the Event |
| `explanation skipped` | warn | `detection`, `namespace`, `owner`, `fingerprint`, `reason`: the call budget is spent, the context does not fit `--max-input-tokens`, the endpoint failed or answered an error status, or the answer did not validate |
| `event write failed` | warn | `fingerprint`, `error`: the Event could not be created or patched; `no answer within 10 seconds` when the whole write (create, and on a repeat its list and series patch) took longer |
| `explanation dropped` | warn | `fingerprint`, `reason`: the queue of 16 incidents waiting for an explanation was full |
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
namespaces, with invented values:

```json
{"ts":"2026-10-28T12:00:00Z","level":"info","msg":"started","namespaces":["payments","shop"],
 "rollouts":true,"server":"v1.36.4","settle_seconds":60,"version":"0.4.0"}
{"ts":"2026-10-28T12:00:01Z","level":"warn","msg":"pod watch error","error":"<kube-rs's text>",
 "namespace":"payments"}
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
- `pods`: the Pods whose failures make up the incident, sorted; empty for a failed Job.
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

With `--metrics-address`, `GET /metrics` returns these counters and the `kausa_leader` gauge in
the Prometheus text format; each counter is `0` until something counts. `GET /healthz` answers
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
| `kausa_explanations_skipped_total` | `reason` (`budget`, `context_too_large`, `endpoint`, `invalid_answer`) | Incidents not explained |
| `kausa_explanations_retried_total` | | Calls tried again after a transient error |
| `kausa_explanations_dropped_total` | | Incidents dropped because the queue of 16 was full |
| `kausa_requests_logged_total` | | Requests logged instead of sent (`--dry-run`) |
| `kausa_redactions_total` | `rule` | Values replaced by redaction |
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

The gauge, rendered after the counters with `# TYPE kausa_leader gauge`:

| Gauge | Labels | Value |
|---|---|---|
| `kausa_leader` | | `1` while this process acts: it holds the Lease, or runs without `--leader-lease`; `0` as a standby (help text: `1 while this process acts (it holds the Lease, or runs without --leader-lease), 0 as a standby.`). Summed over a release's Pods, anything but `1` means nobody acts, or (never expected) two do |

No metric names a Pod, a namespace or the Lease.

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
