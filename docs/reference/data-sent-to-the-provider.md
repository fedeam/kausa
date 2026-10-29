# Data sent to the provider

Exactly what a kausa request to the language model endpoint contains, and what it never
contains.

Status as of 2026-10-29: **implemented** (second implementation weekend, `src/context.rs`,
`src/redact.rs`, `src/prompt.rs`). Why it works this way: [0008. Send only allowlisted, redacted
data to one OpenAI-compatible endpoint](../adr/0008-data-leaving-the-cluster.md) and, for the
Pods' status reason and message, [0016. Send each Pod's status reason and message to the model,
redacted and bounded](../adr/0016-send-the-pod-status-reason-and-message.md) and, for resource
requests, probe handlers, image pull settings and scheduling keys, [0017. Send the model resource
requests, probe handlers, image pull settings and an unschedulable Pod's scheduling keys, reduced
and redacted](../adr/0017-richer-context-for-the-model.md); for a stalled rollout's replica counts
(`D014`), [0018. Explain a Deployment past its progress deadline (`D014`) from a Deployment watch
that is on by default, and add nothing for StatefulSets or DaemonSets](../adr/0018-stuck-rollouts.md).
Run kausa with
`--dry-run` to see the requests it would send ([Try kausa against a cluster with a local
model](../how-to/try-with-a-local-model.md)).

## Where it goes

One endpoint, the base URL given with `--endpoint`, with `POST <base URL>/chat/completions`.
Nothing else: no telemetry, no update checks, no fallback provider. With a model served inside the
cluster or the private network, nothing leaves it.

The model endpoint is the only destination of an incident's context. With notifications on
(`--notify-url-file`, off by default), a second, optional destination, the webhook, receives each
new explanation with the real names of the namespace and the top-level owner, the rule and the
fingerprint, and nothing of this page's request: [Data sent in
notifications](data-sent-in-notifications.md).

## What one request contains

After the allowlist, pseudonyms and pattern redaction of [Detection and redaction
rules](rules.md):

| Field | Example after redaction | Notes |
|---|---|---|
| `detection` | `{"id": "D001", "name": "crash loop"}` | The rule id and its name |
| `workload` | `{"kind": "Deployment", "name": "deployment-1", "namespace": "namespace-1"}` | The top-level owner's kind (a built-in kind as it is, among them `PersistentVolumeClaim` and `HorizontalPodAutoscaler` for `D012` and `D013`; `Object` for a custom resource) and pseudonyms (`R002`) |
| `pods_affected`, `pods_total` | `2`, `2` | |
| `pods` | up to three Pods, the example first; none for an incident about a Job, a workload controller, a claim, an autoscaler or a stalled rollout (`D008`, `D011` to `D014`) | Each with its pseudonym, `phase` and, for `D005`, the scheduler's redacted `unschedulable` message (of the `PodScheduled` condition, for reason `Unschedulable` or `SchedulerError`; the reason itself is not sent) |
| | `reason` `Evicted`, `message` `Pod ephemeral local storage usage exceeds the total limit of containers 50Mi.` | The Pod's `status.reason` and `status.message`, each cut at 1024 bytes and redacted like the other free text (any writer of `pods/status` can set either), when they are set (an eviction, `D009`, or a Pod past its deadline); `null` otherwise. The message names quantities and, for node pressure, a container (pseudonymized, `R002`); for an `emptyDir` limit it names the volume, which kausa does not know and sends as written unless a pattern matches it |
| `pods[].containers` | `name` `container-1`, `init` `false`, `image` `busybox:1.37` or `registry-1/image-1:tag` | Whether it is an init container; public registries kept, others pseudonymized (`R002`) |
| | waiting reason and redacted message; terminated and last termination reasons and exit codes; restart count | As Kubernetes reports them |
| | `memory_request` `16Mi`, `cpu_request` `25m`, `memory_limit` `32Mi`, `cpu_limit` `50m` | Every rule. Each quantity is kept only if it is at most 32 bytes and in the Kubernetes quantity grammar, otherwise `null` (`R001`) |
| | `probes`: `{"kind": "readiness", "handler": "http", "port": 8080, "initial_delay_seconds": 5, "period_seconds": 10, "timeout_seconds": 1, "success_threshold": 1, "failure_threshold": 3}` | Every rule. `kind` is `liveness`, `readiness` or `startup`; `handler` is `exec`, `http`, `https` (scheme `HTTPS`, any case), `tcp`, `grpc` or `null`; `port` is a number from 1 to 65535, a named port resolved to the same container's `containerPort`, or `null` (`exec`, an unresolved name, a number out of range): a port's name is never sent. Never a probe's command, headers, host, gRPC service, or the query and fragment of its path |
| | probe `path` `/ready` | Only `D001` and `D006` (a failing liveness probe ends in `CrashLoopBackOff`): an HTTP probe's path up to its first `?` or `#`, `null` when that is empty or longer than 128 bytes (never cut), redacted like the other free text. Absent for the other rules |
| | `image_pull_policy` `IfNotPresent` | Only `D002`: `Always`, `IfNotPresent` or `Never`, else `null`. Absent for the other rules |
| `pods[].image_pull_secrets` | `1` | Only `D002`: how many entries `spec.imagePullSecrets` has, capped at 16; never a Secret's name, and no Secret is read. Absent for the other rules |
| `warning_events` | up to ten: reason, redacted message, count | Only events a detection matches, of the incident's Pods or of the object the incident is about (a Job, or for `D011` to `D013` the ReplicaSet, Job, StatefulSet, DaemonSet, claim or autoscaler). Their messages can name other objects: a refused Pod, a quota, a container, a StorageClass, an autoscaler's target; `R003` redacts the formats of [Detection and redaction rules](rules.md#redaction-rules) |
| `scheduling` | `{"node_selector": [{"key": "example.com/pool", "value": null}], "node_affinity": [{"term": 0, "key": "kubernetes.io/arch", "operator": "In", "values": ["arm64"], "value_count": 1}], "pod_affinity": [], "pod_anti_affinity": [{"topology_key": "topology.kubernetes.io/zone", "namespaces_set": false}], "tolerations": [{"key": "example.com/dedicated", "operator": "Equal", "effect": "NoSchedule"}], "omitted": 0}` | Only `D005`, once, from the first Pod whose `PodScheduled` condition is `Unschedulable` or `SchedulerError` (the example first); `null` when no Pod has one, absent for the other rules. From the Pod spec: `node_selector` (key order), the required `node_affinity` terms (`matchExpressions` then `matchFields`, operators `In`, `NotIn`, `Exists`, `DoesNotExist`, `Gt`, `Lt` or `other`), the required `pod_affinity` and `pod_anti_affinity` terms (`namespaces_set` tells whether namespaces or a namespace selector are set, never which), and `tolerations` (spec order; operator `Exists`, `Equal` (also when absent) or `other`; effect `NoSchedule`, `PreferNoSchedule`, `NoExecute`, `other` or `null`; a `null` key tolerates every taint). At most 8, 8 (across terms), 4, 4 and 8 entries; `omitted` counts the rest. A key longer than 128 bytes drops its entry (counted); a key that is not a qualified name is sent as `invalid`; the others are redacted like free text, so `eks.amazonaws.com/nodegroup` becomes `host-1/nodegroup` (`R013`). Values only for `kubernetes.io/os` (`linux`, `windows`) and `kubernetes.io/arch` (`amd64`, `arm64`, `arm`, `386`, `ppc64le`, `s390x`, `riscv64`), from those fixed lists, each once; `value_count` counts every value listed, duplicates included |
| `rollout` | `{"replicas": 3, "updated_replicas": 1, "ready_replicas": 2, "available_replicas": 2, "unavailable_replicas": 1, "progress_deadline_seconds": 600, "min_ready_seconds": 0}` | Only for `D014`, and only these seven keys, each an integer or `null` (`replicas`, `progress_deadline_seconds` and `min_ready_seconds` when the Deployment does not set them); the whole value is `null` when kausa no longer holds the Deployment's record. From the Deployment's `spec` and `status`; nothing in it is text, so nothing in it is redacted. No condition message and no `ReplicaFailure`: a refusal reaches the model through `D011`'s redacted event notes. For `D014`, `pods` is empty (`pods_total` counts the Deployment's Pods kausa holds), `warning_events` keeps its filter (no detection matches an event about a Deployment, so it is empty) and `log_tail` is absent |
| `log_tail` | `connect ECONNREFUSED <redacted:ip>:5432` | Only `D001`, `D003` and `D008`: the previous instance for a container that restarted and runs or waits again, otherwise the terminated one; the last 50 lines asked for, cut to the last 4 KiB after redaction; `--no-logs` turns it off |

The whole body of the request: `model`; two messages, the fixed system message and one user
message, `{"untrusted_cluster_data": <the context above>}`, with chat-template markers in it
neutralized; `temperature` `0`; `max_tokens` `800`; and `response_format` with the answer's JSON
schema unless `--no-json-schema` is given. Nothing else.

A field gated to some rules is absent from the requests of the others, never `null`. When a
request is over `--max-input-tokens`, kausa first halves and then drops the log tail, then keeps
one Pod and three events, then sends `scheduling` and every probe `path` as `null`, and only then
skips the explanation (`context_too_large`).

The system message is fixed in the kausa binary and versioned with it. Every value above sits in
one JSON document marked as untrusted data, and the request asks for a JSON answer with a fixed
schema. The provider's own `usage` numbers (tokens in and out) are written in kausa's log and
counted in the metrics (`kausa_model_tokens_total`); the budget's token cap uses kausa's own
estimate of the request, before it is sent. The model gets no tools and no functions.

## What a request never contains

- Secrets and ConfigMaps: kausa cannot read them ([Permissions](permissions.md)).
- Labels and annotations of any object (the exception: for `D005`, an unschedulable Pod's node
  selector, affinity and toleration keys, which name node labels and taints, are sent redacted,
  see `scheduling`), environment variables, command lines and arguments, volume definitions, UIDs,
  resource versions and managed fields.
- Of a Deployment: its Pod template, selector, strategy, revision and conditions (the record kausa
  keeps of it holds the revision and the `Progressing` condition's status and reason only to
  decide `D014` and name its fingerprint).
- A probe's command, headers, host and gRPC service, and the query and fragment of its path; a
  port's name.
- Node selector, node affinity and toleration values, except the fixed `kubernetes.io/os` and
  `kubernetes.io/arch` values; Pod affinity label selectors, `matchLabelKeys`, namespace names and
  namespace selectors; `tolerationSeconds`; preferred affinity terms, topology spread
  constraints, priority and scheduler name.
- Secret names, among them those of `imagePullSecrets`.
- Real names of namespaces, workloads, Pods, containers, nodes and service accounts, and private
  registry hosts.
- Anything a redaction rule matched: IP addresses, email addresses, credentials in URLs,
  authorization values, JSON Web Tokens, PEM blocks, values of secret-named keys, known token
  formats, high-entropy strings and hostnames.
- The cluster's name, its API server address and kausa's configuration.

A known gap, older than 0017: when a probe fails with a connection error, the kubelet's
`Unhealthy` event quotes the probe's URL with the Pod's address, path, query and fragment, and that
event message is redacted by patterns only, so a query value leaves unless a rule matches it. A
path or key leaves as the workload's author wrote it unless a pattern or a known name matches it
([Detection and redaction rules](rules.md#limits)).

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
- With notifications on, everything but the fields of [Data sent in
  notifications](data-sent-in-notifications.md): the log lines, event and status messages, the
  request and its context, the pseudonym mapping and every real name but the namespace's and the
  top-level owner's stay in the cluster; the webhook gets the explanation, which can repeat
  redacted log text.
- The pseudonym mapping and the deduplication cache, in kausa's memory only (fingerprints also
  appear in the log and in the Events' names); nothing is written to disk.
