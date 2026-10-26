# Architecture

How kausa is planned to work: it watches Warning events and Pod status, turns the failures of one
workload into one incident with deterministic rules, sends a redacted context to one
OpenAI-compatible endpoint within a budget, and writes the explanation as an Event on the failing
workload.

Status as of 2026-10-26: the **first implementation weekend is done**: watching, detection, the
settle window, owner correlation and fingerprints run, and `kausa run` logs each incident
([Command line and log format](../reference/command-line.md)). Context, redaction, the model call,
cost controls and the Event output are still the planned design. This page describes the target
for v0.1 and is updated as the code lands. Why kausa exists and what it leaves out:
[0001. Build a small, read-only explainer that writes one redacted explanation per
incident](../adr/0001-scope-and-positioning.md).

## Pipeline

```text
Warning events ─┐
                ├─▶ detect ─▶ settle ─▶ correlate ─▶ incident + fingerprint
Pod status ─────┘                                          │
                                         seen before? ─────┼──▶ re-emit the stored explanation
                                                           ▼
          gather context ─▶ redact ─▶ budget ─▶ model ─▶ validate ─▶ map names back ─▶ Event
```

Only the model call leaves the cluster. Every other stage is deterministic: the same cluster state
produces the same incident, the same fingerprint and the same request bytes.

## Watching without a large cache

- **Two watches:** `events.k8s.io/v1` Events of type `Warning` about Pods and Jobs, and Pods.
  Both through kube-rs watchers, with restarts and backoff handled by the library.
- **No full-object cache.** A reflector that stores every Pod cluster-wide is the usual way to
  grow a controller's memory with the cluster. kausa reduces each watch event to a small record per
  Pod (namespace, name, owner reference, phase, and per container its waiting reason, last
  termination reason, exit code and restart count, and its image, probe types and timings, and
  memory and CPU limits) and drops the rest. Hypothesis to measure: that
  this keeps memory roughly flat with the number of healthy Pods.
- **Server-side filtering.** The event watch uses the field selector
  `type=Warning,reportingController!=kausa`: `events.k8s.io/v1` supports both fields (its field
  label conversion in Kubernetes' `pkg/apis/events/v1/conversion.go`, checked on 2026-10-26).
- **Its own Events are ignored**, by their reporting controller: excluded by the selector, and
  checked again in kausa in case a server does not apply it.
- **Only what can be a signal.** A Warning event is kept only if a detection matches it and it
  happened in the last 10 minutes; messages and notes are cut at 1024 bytes. When a watch
  restarts, it lists again, and records whose objects did not come back are dropped.

## Detection, settle and correlation

- **Detection** is a fixed set of rules over the Pod records and events, listed in [Detection and
  redaction rules](../reference/rules.md): crash loops, image pull failures, out-of-memory kills,
  container creation errors, scheduling failures, probe failures, volume failures and failed Jobs.
  A container matches at most one detection, the most specific one (`D003` before `D001`), before
  grouping.
- **Settle window.** A failure has to persist for a while (60 seconds by default) before it
  becomes an incident. Every five seconds kausa turns its records into signals: states (a failing
  container, a Pod that cannot be scheduled) and recent Warning events. A state settles when it
  has been present over the window, and is forgotten only after 330 seconds without it, so a
  crash loop that runs between back-offs stays one incident. An event settles when the same event
  happens again a window after it was first seen; events of different Pods never add up. A failed
  Job settles at once. The exact rules: [Detection and redaction rules](../reference/rules.md).
- **Correlation.** kausa follows owner references up to the top-level owner. A Pod's reference
  already names its ReplicaSet, StatefulSet, DaemonSet or Job; only the ReplicaSet → Deployment
  and Job → CronJob hops need a `get`, a metadata-only request (PartialObjectMetadata), so
  ReplicaSet and Job specs are never fetched. kausa follows the controller owner reference only
  through kinds it may `get` (ReplicaSet, Job). When the next owner is of another kind, including
  a custom resource, that owner reference is the top-level owner. It is named in the incident key
  and in `regarding` from the reference alone (apiVersion, kind, name, uid), without reading the
  object. Owner lookups have a 5-second timeout and are cached for ten minutes; a Pod's owner is
  resolved again when its controller reference changes. A lookup that fails (a deleted ReplicaSet,
  an API error, a timeout) stops the chain at the ReplicaSet or Job, is logged, and is retried on
  the Pod's next update after 30 seconds.
  Pods of the same owner with the same detection form one incident: three crashing replicas are
  one explanation, not three.
- **Fingerprint.** A hash of the detection, the owner and the normalized message (the image, for
  `D002`), computed before redaction and never sent. It is written in kausa's log and will key
  deduplication.

## Context and redaction

- **Context.** The incident's Pod records, its Warning events, the relevant probe and resource
  settings, and for the detections that read logs the last lines of the failed container, capped
  by lines and bytes: the previous instance (`previous=true`) when the container has restarted
  (`D001`, and `D003` under a restarting policy), and the terminated container itself
  (`previous=false`) for `D008` and any Pod that did not restart (a Job's Pod with
  `restartPolicy: Never` has no previous instance). The exact fields are in [Data sent to the
  provider](../reference/data-sent-to-the-provider.md).
- **Redaction**, in three steps: an allowlist of fields, pseudonymous names, and pattern
  redaction of free text ([0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](../adr/0008-data-leaving-the-cluster.md)). Pseudonyms are numbered in order of
  appearance, never random, so the same incident produces the same request.

## The model call

- **One request per incident**, to `POST <base URL>/chat/completions` of the configured endpoint,
  with one model. No tools, no function calling, no follow-up turns.
- **The prompt.** A fixed system message, versioned with kausa, that says the data is untrusted
  and may contain instructions to ignore; one user message with the redacted context as a JSON
  document. The answer is JSON with a fixed schema: a one-line summary, the likely cause, up to a
  few steps to check or fix, and a confidence level. kausa asks for structured output
  (`response_format` with a JSON schema) where the endpoint supports it, and otherwise relies on
  the instructions and validates.
- **Validation.** The answer must parse and match the schema; strings are length-capped,
  stripped of Unicode control characters (category Cc, except newline) and format characters
  (category Cf, which include the bidirectional overrides U+202A–U+202E and U+2066–U+2069 and the
  zero-width characters) and of markup, then normalized to NFC. The validation fixtures include
  bidirectional and zero-width payloads. An answer that fails validation is dropped and
  counted, never written.
- **Client.** A small HTTP client with TLS through rustls, a connect and total timeout, a cap on
  the response size, one request at a time and a bounded queue (the oldest incident is dropped
  when it overflows, and counted).

## Cost controls

- **Budget.** A cap on calls per hour and per day, a cap on the estimated input tokens of a
  request (the context is cut to fit), and a cap on the answer's tokens (`max_tokens`; whether
  some endpoints, such as OpenAI's reasoning models, need `max_completion_tokens` instead is a
  hypothesis to verify on the second weekend). The budget is global: one budget and one queue for
  the whole cluster, not per namespace. When the budget is spent, incidents wait or are skipped,
  and the metrics say how many.
- **Deduplication.** A bounded cache, in memory, keyed by fingerprint with a time to live: a
  repeated incident gets the stored explanation again, with no new call.
- **Restarts.** The budget counters and the deduplication cache live in memory and start over
  when kausa restarts, so incidents that are still failing are explained again after a restart.
- **Metrics.** Prometheus metrics on their own port: incidents by detection, calls, tokens in and
  out as the provider reports them, cache hits, incidents skipped by the budget, redactions by
  rule, validation failures and errors. Health and readiness endpoints on the same port.

## Output

- **An Event on the failing workload.** `events.k8s.io/v1`, type `Normal`, reason `Explained`,
  `regarding` the top-level owner (a Deployment, StatefulSet, DaemonSet, Job or CronJob, the
  owner of another kind where the chain stops, as in Correlation above, or the Pod when it has
  none) and `related` the Pod the context came from. `kubectl describe deployment <name>` shows
  it, and its `related` field names the Pod whose events it explains (`kubectl describe pod`).
- **Limits of an Event.** The note is limited to 1024 bytes and the reason to 128 bytes (UTF-8;
  the validation compares the string's byte length with Go's `len`; `NoteLengthLimit` and
  `ReasonLengthLimit` in Kubernetes' `pkg/apis/core/validation/events.go`, checked on
  2026-10-23), and the API server keeps Events for one hour by default (`--event-ttl`). kausa
  truncates the note at a UTF-8 character boundary to at most 1024 bytes, and a unit test uses
  multi-byte text. The note carries the summary, the cause and the first steps; while the incident
  lasts, kausa updates the Event's series instead of creating new ones.
- **The full explanation** goes to kausa's own log, with real names mapped back. Longer text and
  history in a custom resource are planned for v0.2.
- **If the explanation is skipped** (budget, validation, provider error), an Event with reason
  `ExplanationSkipped` says why, once per incident.

## Configuration and modes

- One configuration file, mounted from a ConfigMap of kausa's own (kausa reads the file, not the
  API): the endpoint, the model, whether plain HTTP is allowed, the budget, the settle window, log
  collection and the namespaces to watch or skip.
- The API key from a file or an environment variable, which the kubelet fills from a Secret.
- **Dry-run:** the whole pipeline up to the request, which is logged and not sent. No Event is
  written.

## Packaging

- **Binary:** one static executable for linux/amd64 and linux/arm64, with no OpenSSL (rustls).
- **Image:** a minimal base (distroless static or scratch with CA certificates, chosen on the
  third weekend), pinned by digest, running as a numeric non-root user, multi-architecture.
  Target, to be measured: around 10 MB compressed, like the Rust controllers found in the
  [Competitive landscape](../research/2026-10-23-competitive-landscape.md).
- **Install:** a Helm chart with the permissions in [Permissions](../reference/permissions.md), the
  pod security settings and declared requests and limits.
- **Tests:** unit tests for rules, redaction and validation with fixtures; end-to-end tests in a
  kind cluster that create failing workloads, run kausa against a fake model endpoint and check
  the Events; the chart's role checked against the reference.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Clean shutdown after `SIGTERM` or Ctrl-C, or `--help` and `--version` |
| `1` | Runtime failure: kausa cannot reach the API server at start, or a watch ends (watch errors are logged and retried with backoff) |
| `2` | Usage or configuration error: unknown argument, invalid configuration file, cannot write to stdout |

## Safety model

Summarised here, and in full in the [Security policy](../../SECURITY.md):

- **Read-only, except Events.** No Secrets, no ConfigMaps, no write verbs but Events
  ([0009. Run with read-only permissions that never include
  Secrets](../adr/0009-least-privilege-rbac.md)).
- **Nothing leaves unredacted**, and only to the one configured endpoint.
- **Cluster text is untrusted.** Event messages, log lines and names are data inside the prompt;
  the model has no tools, its answer is validated, and kausa never acts on it.
- **Bounded everything.** Records instead of objects, capped log bytes, capped requests and
  answers, a bounded queue and cache, a call and token budget.
