# 0027. Report each settled incident as a fact statement in its own `Detected` Event and webhook message, with or without a model, and mark the model's text as a model's

- Status: accepted
- Date: 2026-11-19
- Stage 1 of: [0026. Report every incident as facts without a model, keep the explanation
  optional, and widen coverage one bounded source at a time](0026-facts-before-the-model.md)
  (its points 1, 2, 4 and 8)
- Amends, when accepted, each with an `Update` paragraph: [0015. Post each new explanation to one
  optional webhook, as JSON, Slack or Telegram](0015-notifications.md) (when a message is sent,
  its bound, its body, which becomes `v2`, and the usage error without `--endpoint`); [0008. Send
  only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md) (the webhook can receive data with no model
  endpoint set); invariant 6 of [ARCHITECTURE.md](../../ARCHITECTURE.md), and a new invariant 12
- Keeps: [0009. Run with read-only permissions that never include
  Secrets](0009-least-privilege-rbac.md) (no permission kausa does not already have; point 7 names
  the one mode that gains the Event writes); [0014. Keep each incident's full explanations in a
  namespaced Explanation resource, off by default](0014-explanation-history.md) (no schema change);
  [0021. Refill the hourly call budget at a steady rate from a start burst of three, and let
  incidents wait for it in the queue, so that a restart does not refill
  it](0021-refill-the-call-budget.md) (the call budget); [0022. Pass the model's answer through the
  pattern redaction rules before it is written anywhere, and redact the query of an HTTP request
  line](0022-redact-the-answer.md); [0025. Replace the model's text that holds one of six listed
  destructive command shapes with a fixed notice, show the notice first, and give that answer `low`
  confidence](0025-withhold-destructive-commands.md) (its notice stays first); and invariants 2 and
  11 as they are worded
- Not decided here: the two cost additions of 0026's point 8, which wait for their numbers
  (point 9)

## Context

Status as of 2026-11-19 (`main` at `6bbeac7`, v0.6.2 released). Nothing below is implemented.

**What already runs without a model.** At each check, `run.rs` counts every settled incident in
`kausa_incidents_total`, writes its `incident` line, and only then, when kausa explains, queues a
job for it. Detection, the settle window, correlation and the fingerprint are the same in every
mode ([Architecture](../explanation/architecture.md)).

**What an incident holds** (`src/incident.rs`): the namespace, the top-level owner (API version,
kind, name, UID), the detection, the fingerprint, when it was first and last seen, the names of
its Pods (the first 100 seen while it was pending and every Pod failing when it settled), and one
sample signal: Pod, container, image, reason, message and exit code. The `incident` line adds
`pods_total`, the Pods kausa holds with that owner ([Command line and log
format](../reference/command-line.md)).

**Which of those values are free text.**

- `sample.message` is a container's waiting message, a Pod's status message, an event's note or,
  for `D014`, the `revision <n>` kausa wrote itself. All but the last are text that whoever can
  run a Pod shapes.
- `sample.image` is a string of the Pod's spec. The API server does not check its syntax: on
  Kubernetes v1.36.4 it refused only an empty one and one with leading or trailing whitespace
  (point 10); not read in the API server's code. The kubelet's `InvalidImageName` reason (`D002`)
  exists because a malformed one reaches a node.
- `sample.reason` is one of the reasons [Detection and redaction rules](../reference/rules.md)
  lists, by construction of `detect_container`, `detect_event` and `rollout_signals`, with one
  exception found for this ADR in `collect_signals`: a crash loop detected by its restart count
  (a container terminated after a restart, with a reason other than `Completed`) carries the
  terminated reason, which the runtime writes. `Error` is the value in kausa's tests; the code
  does not compare it with a list.
- A container's name is a DNS label, per the API reference (hypothesis: not checked in the API
  server's code for this ADR).
- The exit code, the counts and a Deployment's revision are integers.

**Events today** (`src/events.rs`, `src/explain.rs`). One Event per outcome, on the top-level owner,
with the sample Pod as `related`: `Explained` (type `Normal`), whose note is the model's summary,
cause, steps and confidence in 1024 bytes, or `ExplanationSkipped` (type `Warning`). Both are
written in the explanation slot, so after the wait for the budget (0021). A job the queue of 16
drops is logged and counted and gets no Event. So an incident can today leave no Event at all, or
none for minutes, and a reader of `Explained` cannot tell which words kausa read and which a model
wrote.

**Notifications today** ([Data sent in notifications](../reference/data-sent-in-notifications.md)).
Only for an explanation the model wrote: never for a skip, never without `--endpoint` (a usage
error), never in `--dry-run`. The body's `type` is "always `explained` in v1", and within `v1`
"fields are only added, never removed". The messages a webhook acknowledges are bounded by the
call budget, because each follows a model call (0015). Without that, the only bound is the pace
of sends: one at a time, at least 1 second apart (3 for `telegram`), from a queue of 16.

**The history today** ([Explanation resource](../reference/explanation-resource.md)). Every field
of an entry is the model's (`summary`, `cause`, `steps`, `confidence`) or about the call
(`model`, `promptVersion`), and all nine are required.

**Who can cause incidents.** Anyone who can create a workload in a watched namespace. A bare Pod
is its own owner, so a hundred failing bare Pods are a hundred incidents. With a model the call
budget caps what reaches the webhook; facts have no model call to be capped by.

**Permissions today** ([Helm chart](../reference/helm-chart.md)). The chart grants `create` and
`patch` on Events "only with an endpoint, `events` true and no `dryRun`". A release without an
endpoint writes nothing to the cluster.

## Alternatives considered

- **One Event that holds the facts and the model's text.** Either the facts wait for the model's
  call, which the budget or the breaker can hold for minutes and a full queue can drop, or the
  Event is written twice. The statement would also take its bytes out of the 1024 the
  explanation is already cut to.
- **A sentence per detection that says what the failure means**, such as "killed for exceeding
  its memory limit" for `D003`. That is a cause. Hypothesis, not checked: a container also shows
  `OOMKilled` when the node, not its own limit, ran out of memory. The statement gives the field
  and leaves its meaning to the reader, or to the model's marked text.
- **The image in the statement.** For `D002` it is the most useful value there is. It is also
  free text (Context), so it is left out of stage 1: the Event's `related` Pod carries it, and
  `kubectl describe` on that Pod shows it. It can come back in an ADR that says how a string
  nobody validated is shown as a fact.
- **`sample.message`, cut and redacted.** Redaction has false negatives
  ([Security policy](../../SECURITY.md)), and the text is a tenant's. 0026's point 2 excludes it.
- **Copy any reason the cluster gives.** See the exception in the Context: one path carries a
  reason kausa does not enumerate. Decided the other way in point 1.
- **Type `Normal` for `Detected`**, like `Explained`. A person who filters Warnings then misses
  the one Event per workload that kausa adds. Against `Warning`: a cluster that alerts on every
  Warning event gets one more per incident. Chosen: `Warning`; it is what the Event reports.
- **Keep notification `v1` and add `"type": "detected"` without `explanation`.** A receiver
  written for `v1` may rely on `explanation`, and `v1` promised that fields are never removed.
- **One message per incident when a model is on**, by holding the fact until the model answers.
  Facts would wait on the budget and the endpoint, which is what 0026's point 1 rules out.
- **No bound on fact messages but the pace of sends.** That is up to 3,600 an hour into a chat
  channel, at the will of whoever can create Pods.
- **Facts in the Explanation history.** An entry of facts fits no field of `v1alpha1` and all
  nine are required: it needs a schema change and its own ADR. `--history` stays the model's.
- **Write `Detected` where the incident settles**, in the tick of `run.rs`. A slow API server
  would hold the watches there, which is why a dropped job gets no Event today. **Reuse the
  queue of 16**: a registry outage settles more than 16 incidents in one check.

## Decision

Proposed to FedeAM on 2026-11-19; not accepted yet.

### 1. The fact statement

A pure module, `src/fact.rs`, builds one line of text from an incident and `pods_total`. The same
incident gives the same bytes. Its parts, in this order, each left out when its value is absent:

```text
<detection name> (<id>). Sample: container <c>, reason <r>, exit code <n>. Pods seen failing:
<a>. Pods now: <b>. Revision: <v>.
```

(one line; wrapped here). Examples, with invented names:

```text
crash loop (D001). Sample: container web, reason CrashLoopBackOff, exit code 1. Pods seen
failing: 2. Pods now: 3.
image cannot be pulled (D002). Sample: container web, reason ImagePullBackOff. Pods seen
failing: 1. Pods now: 1.
job failed (D008). Sample: reason BackoffLimitExceeded.
rollout does not progress (D014). Sample: reason ProgressDeadlineExceeded. Revision: 7.
```

| Part | From | Allowed when |
|---|---|---|
| Detection name and id | `Detection::name` and `Detection::id`, fixed in the binary | Always |
| `container` | The sample's container | It is a DNS label (lowercase letters, digits and `-`, at most 63 bytes), checked by kausa itself; otherwise left out |
| `reason` | The sample's reason | It equals an entry of a list compiled into kausa: the reasons of [Detection and redaction rules](../reference/rules.md), and `Error` for a crash loop found by its restart count. Any other value is left out, and counted |
| `exit code` | The sample's exit code | An integer, printed in decimal |
| `Pods seen failing` | How many names the incident's Pod set holds | Greater than zero. `at least <a>` when the set holds 100 or more, since the first 100 seen while pending are a cap |
| `Pods now` | `pods_total` | Whenever `Pods seen failing` is printed |
| `Revision` | The unsigned integer of `D014`'s `revision <n>` | `D014` only |

- **Never in a statement:** the sample's message, an event's note, a log line, the image, a
  label, an annotation, a node, and any text a model wrote. The namespace, the owner and the Pod
  are not in the text either: the Event names them in `regarding` and `related`.
- **It names no cause and gives no advice.** `Sample:` means what it means in the `incident`
  line: one signal of the incident, the first in a fixed order, not all of them.
- **Its bytes are letters, digits, spaces and `.,:()-`** and nothing else, so no part of it can
  be a mention, a link, markup or a command. The longest possible statement is under 512 bytes
  (286 in the prototype of point 10, every part at its longest); a unit test pins the number.
- **A rule's name is a label, not a claim.** One says more than its reasons: `D013`, "autoscaler
  cannot get metrics", also covers `FailedGetScale`. The implementation pull request corrects a
  name that the reasons do not bear out, with a changelog line.

### 2. The `Detected` Event

For each settled incident, unless `--no-events`, never in `--dry-run`:

| Field | Value |
|---|---|
| `type`, `reason`, `action` | `Warning`, `Detected`, `Detect` |
| `note` | The fact statement, and nothing else |
| `regarding`, `related` | As today's Events: the top-level owner, and the sample Pod when there is one |
| Name | `kausa.detected.<fingerprint>.<8 hex of the statement's hash>` |
| Reporting controller and instance | As today's Events |

The same statement again patches the Event's series (count and last time), as `Explained` does; a
different statement (another count, another sample) is a new Event. A write that fails is logged
and counted in `kausa_event_write_errors_total`, and not retried, as today.

### 3. The fact path

- At the check where an incident settles, its statement is built and pushed on a **fact queue of
  1024 entries** in memory; nothing is written to the cluster there. An entry is the statement
  and what the Event needs (namespace, owner, sample Pod, fingerprint, first seen): 1,583 bytes
  at the longest names Kubernetes allows, so 1.55 MiB for a full queue, computed. Measured in
  the prototype, the process grew by 1.74 MiB resident when the queue filled (point 10).
- A full queue drops its oldest entry, which is logged (`fact dropped`) and counted
  (`kausa_facts_dropped_total`). The `incident` line was already written.
- **One fact slot** beside the explanation slot and the notification slot writes one Event at a
  time, within the 10 seconds an Event write has today, and then queues the fact's message when
  a webhook is set.
- **A fact never waits for the model, the budget, the breaker or an explanation**, and an
  explanation never waits for a fact. With `--leader-lease`, the writer checks the term before
  each call, as the others do.
- The explanation's job is queued as today, in the queue of 16, and nothing in its path changes
  but the mark of point 4.

### 4. The model's text is marked

- **`Explained` Event:** the model's text starts with the fixed words `Model's explanation, not
  verified: ` (35 bytes of the 1024). For a withheld answer, 0025's notice stays first, then the
  mark, then what is left of the model's text. `ExplanationSkipped` is kausa's own text and does
  not change.
- **Slack and Telegram:** after the header, a line `Facts: …` (point 5), then a line `Model's
  explanation, not verified:`, then today's lines.
- **`json`:** `facts` is kausa's and `explanation` is the model's, by key; the reference page
  says so.
- **History and log:** no change. An `Explanation` object and the `explanation` line hold only
  the model's text, and their pages say whose it is.

### 5. The webhook

`--notify-url-file` no longer needs `--endpoint`. It stays a usage error with `--dry-run`.

**Messages.** One `detected` message per incident, queued by the fact path after its Event (or
at once with `--no-events`). With a model, the `explained` message follows later, as today, and
now carries the facts too. So an explained incident is two messages.

**Body.** The `schema` becomes `kausa.fedeam.com/notification/v2`; a release sends one version
(0015). Against `v1`: `type` is `detected`, `explained` or `capped`; a new object `facts` is in
`detected` and `explained`; `explanation` is only in `explained`.

```json
{
  "schema": "kausa.fedeam.com/notification/v2",
  "type": "detected",
  "kausa_version": "0.7.0",
  "detection": {"id": "D001", "name": "crash loop"},
  "object": {"apiVersion": "apps/v1", "kind": "Deployment", "namespace": "shop", "name": "web"},
  "fingerprint": "5d0c2b7e9a1f4c38",
  "first_seen": "2026-11-19T10:00:00Z",
  "facts": {"reason": "CrashLoopBackOff", "exit_code": 1, "pods_seen": 2,
            "pods_seen_is_minimum": false, "pods_now": 3, "revision": null}
}
```

(invented values; the version is whichever release ships this). `reason`, `exit_code` and
`revision` are `null` when the statement leaves them out.

**Names.** Invariant 11 holds as worded: no real name but the namespace's and the top-level
owner's. The container's name is in the Event and the log, **not** in a notification, in `facts`
or in the text. The text formats' `Facts:` line is the statement without its first sentence and
without the container:

```text
kausa: crash loop (D001) in shop, Deployment web
Facts: reason CrashLoopBackOff, exit code 1. Pods seen failing: 2. Pods now: 3.
fingerprint 5d0c2b7e9a1f4c38
```

**What newly leaves the cluster, and only when a webhook is set:** an enumerated reason, an exit
code, two counts and a revision, beside the namespace, the owner, the detection, the fingerprint
and the time that 0015 already sends. And one new case: a release with a webhook and no model
endpoint now sends those to the webhook, where it sent nothing.

**Bound.** Fact messages have their own, since no model call stands behind them:

- **Once per fingerprint per `--dedup-hours`** (6; with `0`, every occurrence). A table of at
  most 1024 fingerprints, each marked only after a `2xx`, dropping the oldest. So a workload that
  fails, recovers and fails again posts once per window, while its Event's count goes up.
- **At most `--notify-max-per-hour` `detected` messages started in any 60 minutes** (chart value
  `notifications.maxPerHour`; default 30, 1 to 3600). `explained` messages keep 0015's bound,
  the call budget, so a release with both sends at most the sum.
- **Over the cap, the message is not sent**, counted as `capped`, and its Event and log line
  stand. At most once in 60 minutes kausa then posts one fixed `capped` message (`schema`,
  `type`, `kausa_version` and `limit`; as text, `kausa: more than <limit> incidents in the last
  hour; the others are in Events and in kausa's log, not here`), so that the channel never reads
  as the whole picture (0026's point 3).
- Both tables live in memory and hold **per process**: after a restart or a takeover, each
  incident that still fails is posted once more, within the cap.

### 6. Modes

| Mode | `incident` line | `Detected` Event | `detected` message | Model |
|---|---|---|---|---|
| No `--endpoint`, no `--dry-run` | Yes | Yes, unless `--no-events` | With `--notify-url-file` | None |
| `--endpoint` | Yes | Yes, unless `--no-events` | With `--notify-url-file` | As today, marked (point 4) |
| `--dry-run` | Yes | No | Usage error, as today | The request is logged, as today |

Usage errors that go away: `--notify-url-file needs --endpoint`. That stay: `--history needs
--endpoint`, both `--dry-run` ones, and `--model, … need --endpoint or --dry-run`.
`--notify-max-per-hour` without `--notify-url-file` is refused like the other `--notify-*` flags.
`--dry-run` still writes and sends nothing; its `incident` line shows the statement that would be
written.

### 7. Chart and permissions

- **No verb or resource is new.** `create` and `patch` on Events are rendered with `events` true
  and no `dryRun`, **with or without an endpoint**. This is the one widening: a release without
  an endpoint, which today writes nothing to the cluster, starts writing `Detected` Events on
  upgrade. `events: false` keeps it at log lines only.
- `notifications.enabled` no longer needs `model.endpoint`; it is still refused with `dryRun`.
  The NetworkPolicy's webhook rule renders without the endpoint's. New value
  `notifications.maxPerHour`.
- `history.enabled` still needs an endpoint.
- **Stage 1 has no switch of its own** (0026's point 7): `events` turns the `Detected` Event on
  and off together with the other Events, and `notifications.enabled` the messages. With both
  off, kausa is what it is today without an endpoint: log lines, and no write in its role.

### 8. Logs and metrics

- The `incident` line gains `statement`, the exact text, in every mode.
- New lines: `fact dropped` (warn; `fingerprint`, `reason`) and `notification capped` (warn;
  `fingerprint`, `limit`), the second once per capped message.
- `kausa_events_written_total` gains `reason="Detected"`; `kausa_notifications_total` gains
  `outcome="capped"`; new counters `kausa_facts_dropped_total` and
  `kausa_fact_reasons_omitted_total` (a reason outside the list, left out of a statement). Each
  shows its `0` series from the start in the modes that can count it, as the others do
  ([Alerts](../reference/alerts.md) relies on that). No new label names a Pod, a namespace or a
  container.

### 9. The two cost additions wait for their numbers

Neither is part of stage 1's first release. Each is decided by an `Update` here or by its own
ADR, after the measurement named below. One rule is fixed now:

- **A model's text written for one workload is never written on another.** An explanation
  answers one request, with that workload's Pods, limits and probes. Copying it to a second
  workload states about it what nobody looked at. What the same cause across workloads can save
  is therefore the call, never the text: the later workloads keep their `Detected` Event and get
  no explanation, or a fixed note that says where one was written, in the same namespace only.
  - To measure first, in counts only: over a week of `incident` lines on a real cluster and in a
    kind scenario where one registry fails for twenty workloads, how many incidents inside
    `--dedup-hours` share detection, reason and normalized message across owners, and in how many
    of those groups the explanations differed in their cause.
- **A cap in tokens.** The caps in calls already bound tokens: at the defaults, 200 calls a day
  of at most 6,000 input tokens by kausa's estimate and 800 output tokens, so 1,360,000 a day per
  process. `kausa_model_tokens_total` counts only what the endpoint reports.
  - To measure first: over at least 50 calls on each of two servers, whether every answer
    carries `usage`, and how far the reported input is from kausa's estimate. A cap that counts
    reported tokens is proposed only if `usage` is always there; otherwise the note states the
    estimate's error and the cap counts the estimate.

### 10. Bounds, and what is measured before this is accepted

0026's point 6 asks each stage's ADR for its `N` and `B`, and for a research note on a Raspberry
Pi 4 and with 10,000 Pods **before the ADR is accepted**. Stage 1 adds no source, but it adds
two bounded structures, so the same holds for it:

| Structure | `N` | `B` | At most |
|---|---|---|---|
| Fact queue (point 3) | 1024 entries | 1,583 bytes | 1.55 MiB |
| Fingerprints already notified (point 5) | 1024 entries | Under 64 bytes | Under 64 KiB |

Both rows are computed. The prototype measured 1.74 MiB resident for the full queue, and 40 bytes
an entry for the table.

- **Neither `N` is a flag.** 0026 makes `N` a setting for a source's table, where a larger table
  is wider coverage. A queue that empties in seconds and a table of what was already sent widen
  nothing; a queue that drops shows in `kausa_facts_dropped_total`. This reading of point 6 is
  part of what is proposed.
- **The research note is written from a prototype on a branch**, not merged, and this ADR stays
  `proposed` until it is. 0026 also says "before any code": read here as before any code is merged,
  since nothing can be measured without a prototype. Bars fixed now: idle memory within the
  run-to-run spread of v0.6.2 in the same setup; a full fact queue adds at most 2 MiB; writing 1024
  `Detected` Events in a row causes no watch error and no relist. If a bar fails, the numbers above
  change before acceptance, in this text.
- **Idle memory is read as the anonymous memory of the container's control group**, which is the
  process's own. This was not said when the bars were fixed; it is named after the measurement
  in kind, because the `docker stats` figure cannot resolve the bar: between two readings of one
  idle container it moved by -1.34 to +0.41 MiB, and the anonymous memory by -0.04 to +0.08 MiB.
  The note reports both figures.
- **Measured in kind on 2026-11-20**, with the prototype of the unmerged branch
  `proto/0027-fact-statement`: [The fact statement prototype,
  measured](../research/2026-11-20-fact-statement-prototype.md). The three bars hold there; the
  numbers of points 1 and 3 and the hypothesis about the image were corrected in this text from
  it. The measurement on a Raspberry Pi 4 is **pending**, and this ADR stays `proposed` until
  the note has it.

With the implementation, before the release that ships it:

- **Unit tests** pin the statement of every detection (the examples of the reference page are
  compared by tests, as the notification examples are), its byte set and its longest length.
- **A fuzz target `fact`:** for any incident, no panic, only the allowed bytes, within the cap.
- **The kind test**, in both `rbac.scope`s: without an endpoint, a crash loop gives a `Detected`
  Event on its Deployment with the exact note, and the receiver gets a `detected` body; with the
  fake endpoint, `Detected` and then `Explained`, whose note starts with the mark; with a spent
  budget, `Detected` and `ExplanationSkipped`.
- **Hypotheses to check**, each with an `Update` if it fails: the API server refuses a container
  name that is not a DNS label, and accepts any image string but an empty one and one with
  leading or trailing whitespace; `events.k8s.io` accepts the reason `Detected` and the action
  `Detect`; an API server with default admission does not refuse 1024 Event creates sent one at
  a time; which terminated reasons other than `Error` the restart-count path of `D001` meets,
  read in the kubelet's and containerd's code. The first three were checked by behaviour on
  Kubernetes v1.36.4 in the note above, where "any image string" turned out false as first
  worded; the last one is still open.

## Consequences

- **An incident is reported even when its explanation is late, skipped or dropped**, and a
  cluster with no model gets Events and, if set, webhook messages. This is what 0026 asked of
  stage 1.
- **More Events and more messages.** With a model, two Events per incident (`Detected`, then
  `Explained` or `ExplanationSkipped`) and two webhook messages. Events are kept for an hour by
  default, as today.
- **Breaking for `json` receivers:** the schema string changes to `v2`, and a `detected` body
  has no `explanation`. Slack and Telegram texts gain two lines. A changelog entry says so.
- **35 bytes fewer for the model's text** in an `Explained` note, which was already cut at 1024.
  The whole text stays in the history, when on.
- **A release without an endpoint starts writing Events on upgrade** (point 7), and its role
  gains the Event writes. The upgrade notes say how to keep it silent.
- **A statement says less than the log line.** No message, no image. Whoever needs them reads
  the `related` Pod, or kausa's `incident` line, which keeps `sample.message` as today.
- **kausa's own `Detected` and `ExplanationSkipped` Events are Warnings about workloads.** Stage
  2 (Warning events that no detection matches) must leave out the Events kausa reports, or it
  would report its own reports.
- **The description changes with the release that ships this**, as the `Update` of 0026 says:
  the README, the documentation front page, the chart's description and the Artifact Hub entry.
- **This ADR cannot be accepted on reading alone.** Point 10 needs a prototype and a research
  note first, as 0026 decided for every stage.
- **Not verified:** nothing here is merged, and nothing is measured on a Raspberry Pi 4. The
  prototype of point 10 has no webhook message, so the cost of that path is in no figure. The
  defaults of point 5 (30 an hour, 1024 fingerprints) are chosen by analogy with the call budget
  and the deduplication cache, and the hypotheses still open are listed in point 10.
- **When accepted:** `Update` paragraphs in 0015 and 0008. With the implementation: [Command line
  and log format](../reference/command-line.md), [Detection and redaction
  rules](../reference/rules.md) (the statement of each detection, and what kausa watches and
  what it does not, which 0026's point 3 asks for), [Data sent in
  notifications](../reference/data-sent-in-notifications.md) (`v2`),
  [Permissions](../reference/permissions.md), [Helm chart](../reference/helm-chart.md),
  [Alerts](../reference/alerts.md),
  [Architecture](../explanation/architecture.md), [Security policy](../../SECURITY.md) and
  [ARCHITECTURE.md](../../ARCHITECTURE.md): invariant 6 gains "a fact never waits for the model,
  the budget, the breaker or an explanation", and a new invariant 12 says what a fact statement
  may hold (point 1) and that changing it needs an ADR.

Update 2026-11-24: accepted on 2026-11-24, after the measurement on a Raspberry Pi 4 of point 10,
under a plan the maintainer approved on 2026-11-24, and implemented in the same pull request. The
text above is the proposal's of 2026-11-19, with the corrections of 2026-11-20 from the
measurement in kind: its `Date`, the status of its Context, the sentence that opens the
Decision, "Amends, when accepted", "When accepted", "cannot be accepted on reading alone", the
measurement on a Raspberry Pi 4 as pending and "nothing here is merged" are from before. 0015 and
0008 get their `Update` paragraphs, and invariant 6 of [ARCHITECTURE.md](../../ARCHITECTURE.md)
and its new invariant 12 change, with this acceptance. What the acceptance and the implementation
add to the text above:

- **The measurement on a Raspberry Pi 4**, on 2026-11-24, in a k3s cluster of three with about
  100 Pods, the published v0.6.2 and the prototype side by side on one node ([The fact statement
  prototype, measured](../research/2026-11-20-fact-statement-prototype.md#on-a-raspberry-pi-4)):
  the three bars hold. The first, idle memory, was read in the kubelet's `rssBytes`, and the
  note has the check, on those nodes the same day, that `rssBytes` is the anonymous memory of the
  control group point 10 names, to within 20,480 bytes against a spread of v0.6.2 of more than
  250 KiB ([`rssBytes` is the anonymous
  memory](../research/2026-11-20-fact-statement-prototype.md#rssbytes-is-the-anonymous-memory)).
  The 10,000 Pods were measured in kind only; reading 0026's "on a Raspberry Pi 4 and with 10,000
  Pods" as two measurements is the note's interpretation.
- **The fact queue's entry** holds the facts the statement is rendered from, not the rendered
  statement. The 1,583 bytes an entry and 1.55 MiB of points 3 and 10 are the prototype's; a
  unit test pins the implementation's at 1,336 bytes an entry and 1,368,064 bytes (1.30 MiB) for
  a full queue, computed at the same names. No bar changes.
- **`D013` is renamed** "autoscaler cannot read metrics or target scale", as point 1 asks, since
  its reasons include a failure to read the target's scale; its id stays `D013`.
- **Every bound that drops a message owes the `capped` message of point 5**, not only the limit
  per hour: the queue of 16, which `detected` and `explained` messages share, and the fact queue
  of 1024 too, so that the channel never reads as the whole picture after a burst. A `detected`
  message never pushes an `explained` one out of the queue: an explanation stands for a model
  call already spent and is sent once, while a `detected` message's facts stay in its Event and
  in the log, and a burst of failing Pods in one namespace must not evict the explanations of
  others. Messages dropped when kausa stops or its lease term ends owe nothing. The bounds on
  memory are unchanged, so the measurements of point 10 stand. With the wider rule the
  message's text changed from the one point 5 gives; as text, kausa sends `kausa: not every
  incident is posted here (more than <limit> in an hour, or more at once than kausa queues);
  kausa's log has every one`.
- **Within `v2` a value of `type` may be added**, as a field may, and a receiver must ignore a
  message whose `type` it does not know; point 5's `v2` adds no other rule ([Data sent in
  notifications](../reference/data-sent-in-notifications.md#compatibility)).
- **The text formats keep an owner's kind and name on one line.** The API server accepts a line
  break in an ownerReference's kind and name and in an Event's `regarding`, which, rendered as it
  was, forged lines of the text, the mark of point 4 among them; the Slack and Telegram texts
  replace control characters and line separators there with U+FFFD ([Security
  policy](../../SECURITY.md)).
- **A `Detected` Event's name can be taken first.** It follows from the incident, and before it
  patches the series of point 2 kausa checks only the reporting controller, which whoever creates
  an Event sets, so whoever can create Events in a namespace can forge a `Detected` Event there; the `incident`
  line and the `detected` message carry the real statement ([Security policy](../../SECURITY.md)).
- **The kind test asserts both Events of one incident, not their order.** Point 3 lets either be
  written first: a fact never waits for an explanation, and an explanation never waits for a
  fact. So point 10's "`Detected` and then `Explained`", and the "`Detected`, then `Explained`"
  of the Consequences, name the two Events, not an order between them. The test checks that each
  `Explained` Event is named after the fingerprint of a `Detected` Event about the same workload
  and that its note starts with the mark. In kind, an `Explained` Event was created before its
  incident's `Detected` Event when several incidents settled in the same tick. The `detected` and
  `explained` messages are the same: the webhook sends its queue in order, but each message is
  queued by its own path after its Event, so the two of one incident can arrive in either order.

Update 2026-11-24: [0028. Publish each incident as an immutable Incident that only kausa writes, and report by webhook what kurita proposed for it, both off by default](0028-incident-resource-and-remediation-report.md) is proposed, not accepted. It would amend three points of this ADR. Point 3:
with incidents on, the fact slot also creates the incident's Incident, after its `Detected` Event
(at once with `--no-events`) and, for a sample Pod run by a ReplicaSet, after a read of its revision
through the owner lookup, made only for a create kausa will send, which can hold the slot up to 5
seconds more (0028's point 3); the create itself takes the time a write has there. Point 5: the
webhook gains a `remediation` value of `type`, within `v2`, with an hourly bound of its own that the
`detected` count never includes, and that owes no `capped` message. Point 10: each entry of the fact
queue gains one optional pointer, 8 bytes with incidents off and, by an estimate bar 2 of 0028
measures, about 320 bytes with them on, and a table of the Incidents kausa created, at most 500
entries, none dropped before it is 6 hours old, is a new bounded structure; the bar of 2 MiB for a
full fact queue does not change. This takes effect only when 0028 is accepted, which needs its bars
measured and kurita's ADR 0008 accepted; until then the text above holds.

Update 2026-11-24: [0029. Report when kausa stopped seeing an incident's failure, and whether the change of a proposed action was observed, never that the incident was resolved, off by default](0029-incident-end-report.md) is proposed, not accepted. It would amend points 1 and
5, the tracker and the acceptance item on `capped`. Point 1: `Pods now` counts the Pods kausa holds
in the incident's namespace whose resolved owner has the owner's UID; today `pods_of`
(`src/run.rs:1744-1749`, used at `src/run.rs:1208` and `:1692`) counts by UID alone over the Pods of
every watched namespace, so a Pod in another namespace that names the owner's UID inflates it. This
correction applies with every switch off, in the fact statement, the request to the model, the
`pods_total` field of the `incident` log line (`src/run.rs:1213`, the field at `:1384`), the
`Detected` Event and the webhook, and only for an owner whose UID a Pod of another namespace names;
it is the one change of 0029 with every switch off, and it can ship ahead of 0029 in its own change.
Point 5: the webhook gains the `closed` and `outcome` values of `type`, within `v2`, with one hourly
bound for the two that the `detected` count never includes; in the queue an end message is dropped
before any other message and never evicts a `remediation`, and they leave with `remediation`, taken
only when no `detected` and no `explained` message is queued; the `sent` check of `detected`
messages does not apply to them. The `capped` message stays owed only for incidents: an end message
dropped or over its bound owes none, and `kausa_notifications_total` never counts one. The tracker
gains no field: a closing is read from the key it drops and the `first_seen` and `last_signal_at`
its entry already holds; and `observe` stops lowering an entry's `first_seen` once the entry is
emitted, which changes nothing this ADR emits, since only an entry not yet emitted is emitted. This
takes effect only when 0029 is accepted, which needs its bars measured and 0028 accepted, or, for
the correction of point 1, when the change that ships it is merged; until then the text above holds.

Update 2026-11-24: the correction of point 1's `Pods now` shipped ahead of 0029, in its own
change, with every switch off: `Pods now` counts only the Pods of the incident's namespace, in the
fact statement, the request to the model, the `incident` log line, the `Detected` Event and the
webhook. In the same release the owner lookup refuses a name that is not a DNS-1123 subdomain, and
a namespace that is not a DNS-1123 label, with every switch off (0028's point 3): it sends no
`get` for it, and an incident whose owner has such a name names the `ownerReference` or the
Event's object as given.
