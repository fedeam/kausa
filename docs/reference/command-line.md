# Command line and log format

The commands and flags of the `kausa` binary, its exit codes, and the JSON lines it writes.

Status as of 2026-10-26: what the first two implementation weekends built. `kausa run` detects and
groups failures, logs incidents and, with `--endpoint` or `--dry-run`, builds a redacted request
for each one and logs it or the model's explanation; it writes no Event yet ([0001. Build a small,
read-only explainer that writes one redacted explanation per
incident](../adr/0001-scope-and-positioning.md)).

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
| `--endpoint <URL>` | The base URL of an OpenAI-compatible endpoint, such as `https://api.example.com/v1` or `http://127.0.0.1:11434/v1` for Ollama. Each incident's redacted context is sent to `<URL>/chat/completions` and the answer is logged as an explanation. Needs `--model` |
| `--model <NAME>` | The model to ask |
| `--api-key-file <PATH>` | A file whose content (trimmed) is the API key, sent as `Authorization: Bearer`. Without it, the environment variable `KAUSA_API_KEY`; without both, no key. Never an argument |
| `--allow-http` | Allow an `http://` endpoint, for one on the same machine, in the cluster or in the same private network. With `--dry-run` the endpoint is not used, so its scheme is not checked |
| `--dry-run` | Build each request and log it instead of sending it; no endpoint needed |
| `--no-logs` | Do not read container logs (the request has no `log_tail`) |
| `--no-json-schema` | Do not send `response_format`, for servers without structured output; the answer is validated all the same |

Without `--endpoint` or `--dry-run`, kausa only logs incidents. `--model`, `--api-key-file`,
`--allow-http` and `--no-json-schema` alone are usage errors.

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
| `1` | kausa cannot start (no client configuration, the API server is unreachable) or a watch ended |
| `2` | Usage error (unknown argument, invalid flag value, an `http://` endpoint without `--allow-http`, an unreadable API key file), or stdout cannot be written (also while running: `kausa run` stops when it cannot write its log, and says so on stderr) |

## Log lines

`kausa run` writes one JSON object per line on standard output, through a serializer, so a value
taken from the cluster cannot break a line or add a field. Every line starts with `ts` (RFC 3339,
UTC), `level` (`info`, `warn` or `error`) and `msg`; the other fields follow in alphabetical
order. Messages and notes taken from the cluster are cut at 1024 bytes.

| `msg` | Level | Other fields |
|---|---|---|
| `started` | info | `version`, `server` (the API server's version), `settle_seconds` |
| `incident` | info | See below |
| `pod watch error`, `event watch error` | warn | `error`; the watch retries with backoff |
| `owner lookup failed` | warn | `namespace`, `pod` or `job`, `error`; until a retry works (a failed lookup is retried after 30 seconds, on the Pod's next update), the incident names the ReplicaSet or Job instead |
| `stopped` | info | |
| `request` | info | With `--dry-run`: `detection`, `namespace`, `owner`, `fingerprint`, `request` (the exact body that would be sent) and `redactions` (replacements per rule) |
| `explanation` | info | `detection`, `namespace`, `owner`, `fingerprint`, `explanation` (`summary`, `cause`, `steps`, `confidence`, with the real names mapped back), `prompt_version`, `tokens_in`, `tokens_out` (as the endpoint reported them) and `redactions` |
| `explanation skipped` | warn | `detection`, `namespace`, `owner`, `fingerprint`, `reason`: the endpoint failed, answered an error status, or gave an answer that did not validate |
| `explanation dropped` | warn | `fingerprint`, `reason`: the queue of 16 incidents waiting for an explanation was full |
| `log read failed` | warn | `namespace`, `pod`, `container`, `error`; the request is built without the log |
| `stopped on an error`, `cannot start the runtime` | error | `error` |

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

(Wrapped here for reading; kausa writes it on one line.)

- `detection`, `detection_name`: the rule, from [Detection and redaction rules](rules.md).
- `owner`: the top-level owner the Pods were correlated to.
- `pods`: the Pods whose failures make up the incident, sorted; empty for a failed Job.
  `pods_total`: the Pods kausa currently knows with that owner.
- `fingerprint`: 16 hexadecimal digits that stay the same for the same failure of the same
  workload, whichever Pod shows it and however long the back-off has grown ([Detection and
  redaction rules](rules.md)).
- `sample`: the first failing container or event, in a fixed order.

The log carries real names and messages from the cluster, as [Data sent to the
provider](data-sent-to-the-provider.md) says under "What stays in the cluster": who can read
kausa's log sees them.
