# Architecture

How kausa is planned to work: it watches Warning events, Pod status and Deployments, turns the
failures of one workload into one incident with deterministic rules, sends a redacted context to
one OpenAI-compatible endpoint within a budget, and writes the explanation as an Event on the
failing workload.

Status as of 2026-10-26: **the code of the four implementation weekends of v0.1 is done**:
watching, detection, the settle window, owner correlation, fingerprints, the context with log
tails, redaction, the prompt, the client, the validation of answers, dry-run, the call and token
budget, deduplication, the Events on the failing workload and the metrics ([Command line and log
format](../reference/command-line.md)). The image and the Helm chart are built and checked in CI,
not published; the kind test passed on the maintainer's machine on 2026-10-27. Measured on a
Raspberry Pi 4: about 2 MiB of memory ([Memory and CPU on a Raspberry Pi
4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)). This page describes the target
for v0.1 and is updated as the code lands. Why kausa exists and what it leaves out:
[0001. Build a small, read-only explainer that writes one redacted explanation per
incident](../adr/0001-scope-and-positioning.md). Status as of 2026-10-29, the namespaced mode
is released in v0.3.0 ([0013. Offer a namespaced install mode: one watch pair
and one Role per listed namespace, and nothing cluster-wide](../adr/0013-namespaced-install-mode.md)).
Since v0.4.0 (2026-10-30), a third watch, on Deployments, explains rollouts past their progress
deadline (`D014`) ([0018. Explain a Deployment past its progress deadline (`D014`) from a
Deployment watch that is on by default, and add nothing for StatefulSets or
DaemonSets](../adr/0018-stuck-rollouts.md)), and an optional standby replica takes over through a
Lease when the one that acts stops ([Failover](#failover); [0019. Offer an optional standby
replica that takes over through a chart-created Lease, elected by kausa's own
code](../adr/0019-high-availability.md)).

## Pipeline

```text
Warning events ─┐
Pod status ─────┼─▶ detect ─▶ settle ─▶ correlate ─▶ incident + fingerprint
Deployments ────┘                                          │
                                         seen before? ─────┼──▶ re-emit the stored explanation
                                                           ▼
          gather context ─▶ redact ─▶ budget ─▶ model ─▶ validate ─▶ map names back ─▶ Event
```

Only the model call leaves the cluster, and, with notifications on, the post of each new
explanation to one webhook (Output, below). Every other stage is deterministic: the same cluster state
produces the same incident, the same fingerprint and the same request bytes.

## Watching without a large cache

- **Three watches per namespace, or one per kind for the whole cluster:** `events.k8s.io/v1`
  Events of type `Warning` (kept only when a detection matches: about Pods, Jobs, workload
  controllers, PersistentVolumeClaims and HorizontalPodAutoscalers), Pods and, unless
  `--no-rollouts`, `apps/v1` Deployments (two per namespace with it). Without `--namespace`, one
  Pod watch, one event watch and one Deployment watch across all namespaces (`Api::all`); with
  `--namespace`, one of each in every
  listed namespace (`Api::namespaced`), since kube-rs has no watcher of several namespaces and a
  field selector cannot pick several. All through kube-rs watchers, with restarts and backoff
  (0.8 to 30 seconds) handled by the library, except after a `403` (below); each is one HTTP
  connection to the API server.
- **Merged per kind, tagged with their watch.** The Pod watches are merged into one stream with
  `futures`' `select_all`, and so are the event watches and the Deployment watches, each item
  carrying its watch (a namespace, or none for the whole cluster) and each watch ending with an
  "ended" item: the loop keeps one Pod branch, one event branch and one Deployment branch, with no
  task per namespace and no new crate. A watch that ends
  stops kausa with exit code `1` and names it (`the pod watch of namespace shop ended`).
- **No full-object cache.** A reflector that stores every Pod cluster-wide is the usual way to
  grow a controller's memory with the cluster. kausa reduces each watch event to a small record per
  Pod (namespace, name, owner reference, phase, the status reason and message and, for a failed
  Pod, when it failed, and per container its waiting reason, last
  termination reason, exit code and restart count, and its image, memory and CPU requests and
  limits, pull policy, and each probe's type, handler, port, path and timings; the number of pull
  secrets; and, for an unschedulable Pod only, its node selector, affinity and toleration keys, at
  most about 6 KiB whatever the spec holds (a hypothesis until the benchmark is rerun)) and drops
  the rest. Memory still grows with the number of Pods and
  Warning events, by one small record each; with 96 Pods it was about 2 MiB on a Raspberry Pi 4
  ([Memory and CPU on a Raspberry Pi
  4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)), and 31 MiB with 10,000 Pods in
  kind ([Thousands of Pods](../research/2026-10-28-thousands-of-pods.md)).
- **A Deployment is kept only while it is stalled.** Each Deployment is reduced at once to a
  record of what `D014` needs (namespace, name, UID, generation and observed generation, whether it
  is paused, its revision, its deadline, `minReadySeconds`, the desired and status replica counts,
  and the `Progressing` condition's status and reason), never its Pod template, labels, other
  annotations, selector, strategy or any condition message. The record is kept only while the rule
  matches, and dropped on any update that does not, and on delete, so memory grows with stalled
  Deployments, not with Deployments. A list page still holds up to 500 whole objects while it is
  decoded, as for Pods.
- **Server-side filtering.** The event watch uses the field selector
  `type=Warning,reportingController!=kausa`: `events.k8s.io/v1` supports both fields (its field
  label conversion in Kubernetes' `pkg/apis/events/v1/conversion.go`, checked on 2026-10-26).
- **Its own Events are ignored**, by their reporting controller: excluded by the selector, and
  checked again in kausa in case a server does not apply it.
- **Only what can be a signal.** A Warning event is kept only if a detection matches it and it
  happened in the last 10 minutes; messages and notes are cut at 1024 bytes. When a watch
  restarts, it lists again, and records whose objects did not come back are dropped: **per
  watch**, so the relist of one namespace drops only that namespace's unseen records, never
  another's.
- **A refused watch forgets what it saw.** When a watch fails with `403` (its Role or binding is
  gone), kausa can no longer see its objects go away, so it drops that watch's records of that
  kind, in that namespace only (in cluster mode, every record of that kind). Other errors keep
  them, so a transient failure does not reset the settle window. kausa never exits on a refusal
  and never widens its watch: the other namespaces keep working. The refused watch then starts
  over with a new watcher, which lists again once access is back and so brings back the objects
  that did not change meanwhile: kube-rs itself would resume a refused watch from where it was,
  sending only the changes. The delay between tries still grows to 30 seconds across new
  watchers.
- **Only within the namespace an Event is in.** An Event whose `regarding` object is in another
  namespace, or that has none, is ignored, so an incident's namespace is always that of a watched
  Pod or Event, and its owner lookups, log read and Event write stay there.

## Detection, settle and correlation

- **Detection** is a fixed set of rules over the Pod records and events, listed in [Detection and
  redaction rules](../reference/rules.md): crash loops, image pull failures, out-of-memory kills,
  container creation errors, scheduling failures, probe failures, volume failures, failed Jobs,
  evicted Pods, Pod sandboxes that cannot be created, workloads whose Pods are refused, claims that
  cannot be provisioned, autoscalers without metrics, and Deployments past their progress
  deadline. A container matches at most one
  detection, the most specific one (`D003` before `D001`), before grouping.
- **Settle window.** A failure has to persist for a while (60 seconds by default) before it
  becomes an incident. Every five seconds kausa turns its records into signals: states (a failing
  container, a Pod that cannot be scheduled, a Pod evicted in the last 10 minutes) and recent Warning events. A state settles when it
  has been present over the window, and is forgotten only after 330 seconds without it, so a
  crash loop that runs between back-offs stays one incident. An event settles when the same event
  happens again a window after it was first seen; events of different Pods never add up. A failed
  Job settles at once. A stalled rollout (`D014`) is decided after the other signals of the same
  check and yields to an explained incident of the same Deployment, so a crash-looping or refused
  rollout is explained once, by the rule that names its cause. The exact rules: [Detection and
  redaction rules](../reference/rules.md).
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
  `D002`), computed before redaction and never sent to the model; it is in kausa's log, the
  names of its Events and Explanations and, with notifications on, every notification ([Data sent
  in notifications](../reference/data-sent-in-notifications.md#the-fingerprint)). It also keys
  deduplication.

## Context and redaction

- **Context.** The incident's Pod records, its Warning events, the resource requests and limits,
  each probe's handler, port and timings (and its path for `D001` and `D006`), the pull policy and
  pull-secret count (`D002`), an unschedulable Pod's scheduling keys (`D005`), and for the detections that read logs (`D001`, `D003`, `D008`) the last lines of the
  failed container: the previous instance (`previous=true`) when the container restarted and runs
  or waits again, otherwise the terminated container itself (`previous=false`), as for a Job's Pod
  with `restartPolicy: Never`. kausa asks the API server for the last 50 lines (at most 1 MiB),
  streams them with a 10-second timeout, keeps their last 32 KiB with the partial first line
  dropped and invalid UTF-8 replaced, and after redaction cuts the tail to its last 4 KiB. A request carries at most three Pods and ten events. The exact fields are in [Data sent
  to the provider](../reference/data-sent-to-the-provider.md).
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
  normalized to NFC, stripped of Unicode control characters (category Cc, newlines included)
  and of the format characters that matter for display (bidirectional controls U+202A–U+202E and
  U+2066–U+2069, zero-width characters, the byte order mark), and of the markup characters `<`,
  `>` and backticks except in kausa's own `<redacted:…>` markers; whitespace is collapsed. A
  Markdown code fence around the JSON is tolerated. The validation fixtures include
  bidirectional, zero-width and HTML payloads. The real names mapped back into the answer come
  from the cluster, so the texts are cleaned and cut again after that. An answer that fails
  validation is logged as skipped, never written.
- **Template markers.** Chat-template markers in cluster text (`<|im_start|>`, `</s>`,
  `<start_of_turn>`, `[INST]`) get their brackets swapped for look-alikes in the user message, so
  that a server that renders the conversation as text cannot read them as turn boundaries.
- **Client.** reqwest with rustls and the ring provider (the same TLS stack as the Kubernetes
  client), no redirects, no proxy from the environment, a 10-second connect and a 120-second total
  timeout, and a 256 KiB cap on the answer. Plain HTTP only with `--allow-http`.
- **One at a time.** Explanations run one after the other, inside the controller's loop, so a
  slow endpoint never stops the watches (nor does a metrics client: up to 8 connections are
  answered at once, each with 2-second timeouts); incidents wait in a queue of 16, and the oldest is dropped
  (and logged) when it overflows.
- **One retry.** A call that fails with a timeout, a failed connection, `429` or a `5xx` status
  is tried once more 30 seconds later; then the incident is skipped. A call that never reached
  the endpoint is given back to the budget. The start of an error body (at most 512 bytes) goes
  to kausa's log, not to the Event.

## Cost controls

- **Budget.** A cap on model calls per hour (`--max-calls-per-hour`, 30 by default) and per day
  (`--max-calls-per-day`, 200), counted over sliding windows; a cap on the estimated input tokens
  of a request (`--max-input-tokens`, 6000, estimated as a quarter of the request's bytes): the
  log tail is halved until the request fits, then all but the first Pod and three events are
  dropped, and an incident that still does not fit is skipped; and `max_tokens` 800 on the answer
  (whether some endpoints, such as OpenAI's reasoning models, need `max_completion_tokens` instead
  is still to verify). The budget is global: one budget and one queue for the whole cluster, not
  per namespace. An incident over the budget is skipped, logged and counted, with an
  `ExplanationSkipped` Event.
- **Deduplication.** A cache in memory of up to 1024 explanations, keyed by fingerprint, for
  `--dedup-hours` (6 by default): a repeated incident gets the stored explanation again, logged
  with `"cached": true` and written as an Event, with no new call.
- **Restarts.** The budget counters and the deduplication cache live in memory and start over
  when kausa restarts, so incidents that are still failing are explained again after a restart.
- **Metrics.** With `--metrics-address`, Prometheus counters on their own port: incidents by
  detection, model calls by outcome, tokens in and out as the endpoint reports them, cache hits,
  skipped explanations by reason, dropped ones, logged requests, redactions by rule, and Events
  written and failed; `/healthz`, and `/readyz` once every watch has listed (three, or three per
  watched namespace; two with `--no-rollouts`), never going back. A minimal HTTP/1.1
  responder on `tokio::net`, with no framework. The list: [Command line and log
  format](../reference/command-line.md#metrics).

## Output

- **An Event on the failing workload.** `events.k8s.io/v1`, `regarding` the top-level owner (a
  Deployment, StatefulSet, DaemonSet, Job or CronJob, the owner of another kind where the chain
  stops, as in Correlation above, or the Pod when it has none) and `related` the Pod the context
  came from: type `Normal` and reason `Explained` for an explanation, type `Warning` and reason
  `ExplanationSkipped` (with why) when the budget, the endpoint or the validation stopped it, or
  the request did not fit `--max-input-tokens`.
  `kubectl describe deployment <name>` shows it next to the Deployment's own events (seen on a live
  cluster on 2026-10-26). Action `Explain`, reporting controller `kausa`, reporting instance the
  Pod's name (`POD_NAME`). Written unless `--no-events`, never in dry-run.
- **One Event per incident and text.** An Event's note cannot change once it is created (the API
  server validates it as immutable), so the name is `kausa.<fingerprint>.<hash of the note>` (or
  `kausa.skipped.<fingerprint>.<hash>`): when the same incident comes back with the same text, as
  with an explanation from the deduplication cache, kausa patches only the existing Event's
  series (count and last time) instead of adding an Event; new text is a new Event. It patches
  only an Event whose reporting controller is `kausa`.
- **Limits of an Event.** The note is limited to 1024 bytes and the reason to 128 bytes (UTF-8;
  the validation compares the string's byte length with Go's `len`; `NoteLengthLimit` and
  `ReasonLengthLimit` in Kubernetes' `pkg/apis/core/validation/events.go`, checked on
  2026-10-23), and the API server keeps Events for one hour by default (`--event-ttl`). The note
  is the summary, `Likely cause:` and the numbered steps, cut at a UTF-8 character boundary with an
  ellipsis when it does not fit.
- **The full explanation** goes to kausa's own log, with real names mapped back.
- **An Explanation beside the Event**, with `--history` (off by default; since v0.3.0): the whole text, not cut to 1024 bytes and not deleted after an
  hour, in an `Explanation` object named `kausa.<fingerprint>` in the failing workload's namespace,
  so the Event's name leads to it. It keeps up to five distinct texts with their counts and times,
  is written after the Event and independently of it (a failure of one never stops the other),
  and is deleted by kausa's own sweep after `--history-days` or beyond 500 objects. The writer
  reads its object back as untrusted input and never logs or sends what it read ([Explanation
  resource](../reference/explanation-resource.md), [0014. Keep each incident's full explanations
  in a namespaced Explanation resource, off by default](../adr/0014-explanation-history.md)).
- **A notification**, with `--notify-url-file` (off by default; since v0.3.0): each explanation the model wrote is posted to one webhook, as a documented
  `json` body, a Slack incoming webhook's plain text or a Telegram `sendMessage`, with the real
  names of the namespace and the top-level owner only; every other name stays a placeholder
  (`Redactor::restore_only`). It is queued after the Event, never for a skip, and sent by its own
  task beside the watches, one at a time from a queue of 16, with one retry 30 seconds later
  after a transient failure, so a slow or dead webhook never delays an explanation or an Event. A
  cached repeat is posted again only until the webhook acknowledges it ([Data sent in
  notifications](../reference/data-sent-in-notifications.md), [0015. Post each new explanation
  to one optional webhook, as JSON, Slack or Telegram](../adr/0015-notifications.md)).

## Configuration and modes

- Command-line flags, which the Helm chart sets ([Helm chart](../reference/helm-chart.md)): the endpoint, the model, whether plain HTTP is
  allowed, the settle window, log collection and structured output ([Command line and log
  format](../reference/command-line.md)). The configuration file planned at first was dropped on
  2026-10-26: flags need no parser and no ConfigMap, and keep what kausa is told to do visible in
  its Pod spec. The budget is set with flags too. kausa watches every namespace, or with
  `--namespace` (repeatable, up to 32) only the ones listed; the chart's `rbac.scope=namespaced`
  passes `rbac.namespaces` as those flags and grants a Role in each, so the watch and the
  permissions come from one list. The budget, the queue and the deduplication cache stay one per
  process, shared by every watched namespace.
- The API key from a file (`--api-key-file`, a Secret mounted by the kubelet) or the environment
  variable `KAUSA_API_KEY` (filled from a Secret), never from an argument. The webhook URL and its
  optional bearer token only from files (`--notify-url-file`, `--notify-token-file`), never from
  an argument or the environment.
- **Dry-run:** the whole pipeline up to the request, which is logged and not sent. No Event is
  written.

## Failover

Off by default. With the chart's `replicaCount: 2`, two Pods run `kausa run --leader-lease
<release namespace>/<full name>-leader`, and only the one that holds that Lease runs the pipeline
above. The other is a standby: it connects, logs `started`, serves its metrics and reads the Lease
every 5 seconds, and nothing else (no watch, owner lookup, model call or write). Both are Ready,
the standby answering `standby` on `/readyz` ([Command line and log
format](../reference/command-line.md#leader-election)).

**One process, one task.** kausa runs on one thread with no spawned task, so the election is not a
background thread. In the leading phase one top-level `tokio::select!` polls, in this order, the
signals, the Lease keeper, and then, at random, the controller, the metrics `accept` and the open
metrics connections (as a single replica shares them). The keeper ends on another holder or on its
own deadline timer, which a hung call cannot delay, and when it ends, the controller future is
dropped with everything in flight. A flood on the metrics port therefore takes no more of the
controller's share than with one replica, and cannot keep a starved leader holding the Lease
(how much it takes under the 200m CPU limit is not measured yet). Stepping down never exits:
`Budget` and `Dedup` (with the notified marks) are kept for the next term; the records, the
tracker, both queues, the delayed retries and the owner cache are dropped.

**Timings,** constants (production values), on each replica's monotonic clock only:

| Constant | Value | Meaning |
|---|---|---|
| `LEASE_DURATION` | 30 s | A standby takes the Lease once it has seen it unchanged this long |
| `RENEW_DEADLINE` | 20 s | The leader acts until this long after it *sent* its last good renewal |
| `RETRY_PERIOD` | 5 s | The leader renews and the standby reads this often |
| `CALL_TIMEOUT` / `RELEASE_TIMEOUT` | 5 s / 2 s | Each Lease call / the release at a stop |
| `WRITE_BOUND` | 10 s | The longest an Event write, a history write or sweep call, or a webhook attempt may run |

The arithmetic: let `sent` be when the leader sent its last renewal that succeeded. The API server
applied it at `a >= sent`; a standby sees the new version at `o >= a` and may take over only at
`o + 30 s >= sent + 30 s`. The leader starts a write only while `now < sent + 20 s` (the term
guard, checked before each budget take, Event call, history call, sweep call and webhook attempt),
and every write ends, or is abandoned by the client, within 10 s: by `sent + 30 s`. A compile-time
assertion keeps `RENEW_DEADLINE + WRITE_BOUND <= LEASE_DURATION`. This rests on two hypotheses:
the API server does not apply a request after the client gave up on it, and a paused VM's
monotonic clock advances. There is no fencing token.

So a crashed or partitioned leader is replaced within 40 s of its last renewal (the standby sees
the change within 5 s, then waits 30 s, and one more read of 5 s when its check falls just
short), and a deleted or drained one in about 5 s, since it releases the Lease on `SIGTERM`, or
less when its replacement Pod takes it first. Then the new leader lists (from start to Ready,
about 0.5 s with 32 namespaces and 1.6 s with 10,000 Pods in kind: [Thirty-two
namespaces](../research/2026-10-28-32-namespaces.md), [Thousands of
Pods](../research/2026-10-28-thousands-of-pods.md)) and its incidents settle (60 s by default). With one replica, a lost node leaves nothing explained until the Pod is replaced, about
five minutes with the default `tolerationSeconds` of 300 (hypothesis) plus start-up and the settle
window. These takeover times are the design's; their measurement in kind is an open item (the
research note that 0019 makes a condition of shipping).

**What a failover costs:**

| State | On failover | Bound |
|---|---|---|
| Records, settle progress (the tracker) | Rebuilt by the new leader's list | A still-failing Pod-state incident is explained `settleSeconds` after the list; an Event-based one as soon as its Warning repeats; a `D014` state settles again |
| Explanation queue (16), delayed retries | Lost | The incidents come back if they still fail |
| The explanation in flight | Its call is spent, its answer dropped | One call |
| Notification queue (16) | Dropped (counted `dropped`) | 16 |
| `Dedup` and the notified marks | Per process; the new leader's is its own (empty, or from its last term) | One more call, Event series patch or new Event, history merge and notification per still-failing incident |
| `Budget` | Per process, kept across its terms | At most twice `callsPerHour` and `callsPerDay` per release while both processes live; a restarted container or a new Pod starts over, as with one replica |
| Owner cache | Rebuilt | One `get` per ReplicaSet or Job |
| Counters | Per process | Dashboards sum over the Pods |

The history ([Explanation resource](../reference/explanation-resource.md)) keeps an incident's
record across a failover, in the same object, and is never read back into `Dedup` or `Budget`.

**Failing closed.** kausa never creates its Lease. When nobody can hold it, nobody acts: a Lease
held under another identity, or whose version changes faster than every 30 s, by whoever can
write Leases in the release namespace; a deleted Lease or `<full name>-leader` RoleBinding; or
Lease calls failing for 20 s, as in an API server outage, which also drops the records and settle
progress a single replica keeps. Each shows as `sum(kausa_leader) != 1` over the Pods ([Install
kausa with Helm](../how-to/install-with-helm.md#run-a-standby-replica)).

**Cost:** about 0.4 API requests per second (a read and a renewal every 5 s), and a second Pod
(10m CPU and 32 MiB requested).

## Packaging

Decided on 2026-10-26: [0010. Ship a static binary on distroless, with a Helm chart, built in CI
and not published yet](../adr/0010-packaging.md).

- **Binary:** one static executable, linked against musl, for linux/amd64 and linux/arm64, with no
  OpenSSL (rustls with ring): a release build with LTO, stripped, that aborts on a panic.
- **Image:** the binary alone on `gcr.io/distroless/static-debian13:nonroot` (CA certificates and
  the non-root user, no shell), pinned by digest, running as user 65532. Each architecture is
  built natively. 4.3 MB compressed for linux/arm64 on 2026-10-26, under the 10 MB of the Rust
  controllers found in the [Competitive
  landscape](../research/2026-10-23-competitive-landscape.md). Built in CI, not published.
- **Install:** the Helm chart in `charts/kausa`, with the permissions in
  [Permissions](../reference/permissions.md) (only those the settings use), the restricted Pod
  Security settings, and declared requests and limits ([Helm chart](../reference/helm-chart.md)).
- **Tests:** unit tests for rules, redaction, validation, the budget, Events and metrics with
  fixtures; the chart linted and rendered in CI; an end-to-end test in kind (`tests/e2e/run.sh`)
  that installs the chart, creates failing workloads, runs kausa against a fake model endpoint,
  and checks the Events, that no real name reached the endpoint, the chart's role against the
  reference, and the metrics, once per `rbac.scope` (`KAUSA_E2E_SCOPE`), and once more in the
  namespaced scope with a standby (`KAUSA_E2E_REPLICAS=2`): graceful, frozen and fail-closed
  handovers.

## Exit codes

`0` on success or a clean stop, `1` when kausa cannot start or a watch ends, `2` on a usage error
or when stdout cannot be written. The full table, with every usage error: [Command line and log
format](../reference/command-line.md#exit-codes).

## Safety model

Summarised here, and in full in the [Security policy](../../SECURITY.md):

- **Read-only, except Events.** No Secrets, no ConfigMaps, no write verbs but Events
  ([0009. Run with read-only permissions that never include
  Secrets](../adr/0009-least-privilege-rbac.md)), kausa's own Explanations with the history, and,
  with a standby, a merge patch of the `spec` of its one Lease.
- **Nothing unredacted reaches the model**, and nothing goes anywhere but the configured
  endpoint and, with notifications on, the configured webhook, which gets only the fields of
  [Data sent in notifications](../reference/data-sent-in-notifications.md).
- **Cluster text is untrusted.** Event messages, log lines and names are data inside the prompt;
  the model has no tools, its answer is validated, and kausa never acts on it.
- **Bounded everything.** Records instead of objects, capped log bytes, capped requests and
  answers, a bounded queue and cache, a call and token budget.
