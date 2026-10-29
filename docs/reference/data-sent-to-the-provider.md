# Data sent to the provider

Exactly what a kausa request to the language model endpoint contains, and what it never
contains.

Status as of 2026-10-28: **implemented** (second implementation weekend, `src/context.rs`,
`src/redact.rs`, `src/prompt.rs`). Why it works this way: [0008. Send only allowlisted, redacted
data to one OpenAI-compatible endpoint](../adr/0008-data-leaving-the-cluster.md) and, for the
Pods' status reason and message, [0016. Send each Pod's status reason and message to the model,
redacted and bounded](../adr/0016-send-the-pod-status-reason-and-message.md). Run kausa with
`--dry-run` to see the requests it would send ([Try kausa against a cluster with a local
model](../how-to/try-with-a-local-model.md)).

## Where it goes

One endpoint, the base URL given with `--endpoint`, with `POST <base URL>/chat/completions`.
Nothing else: no telemetry, no update checks, no fallback provider. With a model served inside the
cluster or the private network, nothing leaves it.

## What one request contains

After the allowlist, pseudonyms and pattern redaction of [Detection and redaction
rules](rules.md):

| Field | Example after redaction | Notes |
|---|---|---|
| `detection` | `{"id": "D001", "name": "crash loop"}` | The rule id and its name |
| `workload` | `{"kind": "Deployment", "name": "deployment-1", "namespace": "namespace-1"}` | The top-level owner's kind (`Object` for a custom resource) and pseudonyms (`R002`) |
| `pods_affected`, `pods_total` | `2`, `2` | |
| `pods` | up to three Pods, the example first | Each with its pseudonym, `phase` and, for `D005`, the scheduler's redacted `unschedulable` message (of the `PodScheduled` condition, for reason `Unschedulable` or `SchedulerError`; the reason itself is not sent) |
| | `reason` `Evicted`, `message` `Pod ephemeral local storage usage exceeds the total limit of containers 50Mi.` | The Pod's `status.reason` and `status.message`, each cut at 1024 bytes and redacted like the other free text (any writer of `pods/status` can set either), when they are set (an eviction, `D009`, or a Pod past its deadline); `null` otherwise. The message names quantities and, for node pressure, a container (pseudonymized, `R002`); for an `emptyDir` limit it names the volume, which kausa does not know and sends as written unless a pattern matches it |
| `pods[].containers` | `name` `container-1`, `init` `false`, `image` `busybox:1.37` or `registry-1/image-1:tag` | Whether it is an init container; public registries kept, others pseudonymized (`R002`) |
| | waiting reason and redacted message; terminated and last termination reasons and exit codes; restart count | As Kubernetes reports them |
| | `memory_limit` `32Mi`, `cpu_limit` `50m`; each probe's kind and timings | Numbers only; never a probe's command, path or headers |
| `warning_events` | up to ten: reason, redacted message, count | Only events a detection matches, of the incident's Pods or Job |
| `log_tail` | `connect ECONNREFUSED <redacted:ip>:5432` | Only `D001`, `D003` and `D008`: the previous instance for a container that restarted and runs or waits again, otherwise the terminated one; the last 50 lines asked for, cut to the last 4 KiB after redaction; `--no-logs` turns it off |

The whole body of the request: `model`; two messages, the fixed system message and one user
message, `{"untrusted_cluster_data": <the context above>}`, with chat-template markers in it
neutralized; `temperature` `0`; `max_tokens` `800`; and `response_format` with the answer's JSON
schema unless `--no-json-schema` is given. Nothing else.

The system message is fixed in the kausa binary and versioned with it. Every value above sits in
one JSON document marked as untrusted data, and the request asks for a JSON answer with a fixed
schema. The provider's own `usage` numbers (tokens in and out) are written in kausa's log and
counted in the metrics (`kausa_model_tokens_total`); the budget's token cap uses kausa's own
estimate of the request, before it is sent. The model gets no tools and no functions.

## What a request never contains

- Secrets and ConfigMaps: kausa cannot read them ([Permissions](permissions.md)).
- Labels, annotations, environment variables, command lines and arguments, volume definitions,
  UIDs, resource versions and managed fields.
- Real names of namespaces, workloads, Pods, containers, nodes and service accounts, and private
  registry hosts.
- Anything a redaction rule matched: IP addresses, email addresses, credentials in URLs,
  authorization values, JSON Web Tokens, PEM blocks, values of secret-named keys, known token
  formats, high-entropy strings and hostnames.
- The cluster's name, its API server address and kausa's configuration.

## What stays in the cluster

- kausa's own log, with real names: the `incident` lines; with `--dry-run`, the `request` lines
  (the redacted body, next to the real namespace and owner); the `explanation` lines (real names
  mapped back); and the `explanation skipped` and `log read failed` lines (the real namespace,
  owner, Pod and container). Whoever can read or ships kausa's log sees them ([Command line and log
  format](command-line.md)).
- A short version of the explanation, up to the 1024-byte limit of an Event note, in an Event
  about the failing workload, with real names, visible to whoever can read Events in that
  namespace (unless `--no-events`).
- With `--history`, the whole explanation, with real names, in an Explanation object in the
  failing workload's namespace ([Explanation resource](explanation-resource.md)). Nothing stored
  there is sent to the provider or read back into a request, the cache, an Event or the log.
- The pseudonym mapping and the deduplication cache, in kausa's memory only (fingerprints also
  appear in the log and in the Events' names); nothing is written to disk.
