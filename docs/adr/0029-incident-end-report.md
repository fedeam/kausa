# 0029. Report when kausa stopped seeing an incident's failure, and whether the change of a proposed action was observed, never that the incident was resolved, off by default

- Status: proposed. Acceptance needs the bars of "Conditions of acceptance" measured, and
  [0028](0028-incident-resource-and-remediation-report.md) accepted, since everything here reads
  its table of created Incidents. Until then kausa reports no end of an incident, and the ADRs this
  one amends hold as written.
- Date: 2026-11-24
- Builds on: [0026. Report every incident as facts without a model, keep the explanation optional,
  and widen coverage one bounded source at a time](0026-facts-before-the-model.md) (its point 3,
  what kausa never says), [0027. Report each settled incident as a fact statement in its own
  `Detected` Event and webhook message, with or without a model, and mark the model's text as a
  model's](0027-fact-statement-for-the-detections.md) (the tracker, the webhook's `v2` and its
  queue) and [0028. Publish each incident as an immutable Incident that only kausa writes, and
  report by webhook what kurita proposed for it, both off by default](0028-incident-resource-and-remediation-report.md)
  (the table of created Incidents, the checks of a Remediation, the `remediation` message and the
  attach point of its point 9)
- Amends, when accepted, each with an `Update 2026-11-24` paragraph at its end: [0015. Post each
  new explanation to one optional webhook, as JSON, Slack or Telegram](0015-notifications.md) (two
  new kinds of message); [0026](0026-facts-before-the-model.md) (its point 3); [0027](0027-fact-statement-for-the-detections.md)
  (its point 5 and its acceptance item on `capped`); [0028](0028-incident-resource-and-remediation-report.md)
  (its points 1, 4, 5 and 9); and invariants 5, 11 and 12 of [ARCHITECTURE.md](../../ARCHITECTURE.md),
  changed with the code
- Keeps: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md) (nothing new goes to the model) and [0009. Run with
  read-only permissions that never include Secrets](0009-least-privilege-rbac.md) (no permission
  changes, with the switches on or off)

kurita is a separate project, a controller that proposes a remediation for an incident kausa
reports; its ADR 0008, the contract with kausa, is the other half of 0028. This ADR closes the loop
the maintainer asked for: kausa detects and publishes the incident, kurita re-checks it and
proposes, and kausa then says, by its webhook, how the incident ended. **kausa cannot know that an
incident was resolved, and never says so.** It says two narrower things, each typed: that it
stopped seeing the failure, on which basis and when; and, after kurita proposed one of three
actions that change a workload, whether it saw the change that action makes on every Pod of the
workload within a stated window. In kurita v0.1 nothing executes, so an `observed` is always a
change someone else made, by hand or otherwise. Every figure of memory, size and time below is
**computed, not measured**, unless it says measured.

## Context

Status as of 2026-11-24: `main` at `588f3b5`, v0.6.3 released. 0027 is `proposed` on `main`, and
the change that implements and accepts it is not merged that day; every path and line cited below
is that change's. 0028 is `proposed`, with kurita's ADR 0008, "Contract with kausa", as its other
half. Nothing below is implemented. kurita is not published as of 2026-11-24, so its ADRs are
cited by number and title, without links.

**What kausa does today when a failure stops.** Nothing visible. The tracker forgets a key at the
tick where it drops it from `pending` (`Tracker::tick`, `src/incident.rs:383-387`): a state key
(`is_state`, `src/detect.rs:98-115`: `D001` to `D005`, `D009` and `D014`) when no tick counted its
signal for more than `STATE_GRACE`, 330 seconds (`src/incident.rs:23`), and an event key at the
first tick with no fresh matching Event. No log line, Event or message marks it. The incident's
`Detected` Event and its `detected` message stand, and a reader cannot tell an incident that ended
from one still going.

**What the request needs, and what kausa may say.** 0026's point 3 forbids saying that something is
healthy, that nothing is wrong, or that kausa saw everything. So "the incident ended" cannot be a
verdict on the workload. It can only be a statement about kausa's own observation (it stopped
counting the signal), and, when kurita proposed an action that changes the workload, about a typed
field of the Pods that the action would change.

**What makes "the rule stopped matching" a weak end.** `D009` stops matching by itself: an
eviction counts as a signal for 10 minutes after the Pod failed (`EVENT_FRESHNESS`,
`src/run.rs:80`, applied at `src/incident.rs:119-131`), whoever acted. An event-born detection
stops at most 10 minutes after the last occurrence of its Event (`src/incident.rs:175-180`), and
earlier when the Event or its Pod is gone (`:194-197`); for `D011`, `D012` and `D013`, Kubernetes
was seen to stop updating the Event while the failure was still there ([Detection and redaction
rules](../reference/rules.md), its `D011`, `D012` and `D013` sections; the cause is a hypothesis
there). So a closing says what kausa stopped seeing, and the outcome of an action needs a check of
its own.

**How the design was reached.** As for 0028: two complete designs answered the same brief and
fixed limits, and a review chose between them point by point, checking each one's claims against
kausa's source, kurita's ADRs and the Kubernetes source. The design below takes most of its shape
from one (no `Incident.status` write, a `closed` message, the `source-lost` closing, keyed hashes)
and its outcome check from the other (a window with a deadline, every Pod). The review left twelve
choices to the maintainer; all twelve stand as it made them, and "Alternatives considered" gives
each one's alternative.

The fixed limits of that brief, which this ADR keeps: kausa executes nothing and reads no Secret
and no ConfigMap; nothing new goes to the model, and nothing of the closing or the outcome comes
from it; no free text written by another controller leaves by the webhook; what leaves about a
remediation rests on a resource only the two controllers write and on live objects kausa reads
itself, never on an Event alone; with incidents off kausa behaves as before 0028; the two projects
couple only through cluster resources.

## Alternatives considered

- **Writing the end into `Incident.status`**, the slot 0028's point 9 reserved, with `patch` on
  `incidents/status`. Rejected; the slot stays declared, empty and reserved. The review found in the
  Kubernetes source that the design as proposed does not hold: a merge patch to `/status` that
  carries `metadata.uid` is not refused when the UID differs, the UID is ignored (the patch handler
  passes no preconditions, and the status strategy copies the whole old object over the new one),
  so after a delete and a new create under the same name kausa would patch the status of an
  object it did not create; and a rule placed on `status` does not run when a merge patch removes
  `status` itself (`status: null`), so a written result could be erased. Both can be fixed (a
  `resourceVersion` precondition, a root rule `!has(oldSelf.status) || has(self.status)`), and
  kurita would have to parse only `metadata` and `spec`. What remains costs `patch` on
  `incidents/status`, two writes per Incident, one `MODIFIED` event per write to kurita's watch,
  and a way for any holder of `*/status` to write a result first and make kausa send nothing. What
  it buys is that `kubectl get incident` shows the end, for the day an Incident is kept; the
  webhook receiver, who asked, gets nothing from it.
- **A snapshot of the Pods at closing instead of an outcome over a window.** Simpler, and it needs
  no pending check. But a failure that never stops gets no outcome, which is the case the request
  most needs; and with "any Pod shows the change", one forged Pod whose `ownerReference` names an
  existing ReplicaSet decides the result, since kausa resolves a Pod's owner by the ReplicaSet's
  name (`top_owner`, `src/owner.rs:52-69`).
- **"Any Pod shows the change" instead of every non-terminal Pod.** It gives `observed` earlier, in
  the middle of a rollout, and one Pod can forge it.
- **A bound of its own for each of `closed` and `outcome`.** 30 more messages an hour at the
  default, for no reader who needs them apart. Or sharing the bound of `remediation` messages,
  which would let closings of forged Event incidents use up the bound of genuine `remediation`
  messages. One shared bound for the two end messages instead (point 5).
- **End messages that owe the `capped` message.** The `capped` text speaks of incidents not posted
  and of kausa's log having every one (`src/notification.rs:170-175`); an end message is not an
  incident, so it would need a new text. They owe none, as 0028 decided for `remediation`.
- **A closing for `D008` too**, at a fixed time after the Job failed. A failed Job is a terminal
  fact (`settles`, `src/detect.rs:91-93`): a clock notice would say nothing kausa observed. Or no
  closing for a CronJob's `D011` either, since it reopens at each schedule; it does close, because
  its Event does stop.
- **Posting `detected` again when a closed incident comes back** inside `--dedup-hours`. It changes
  0027's dedup and costs one more `detected` per fingerprint per window, and it says nothing a
  receiver cannot infer from a `closed` and a later Event. A returning failure stays silent until
  the window ends, as today.
- **Leaving `none` out of the `closed` message,** as 0028 leaves it out of `remediation` messages.
  Then `recorded: no` would misstate that kurita recorded something. `none` is shown as
  `recorded: yes, action: none`.
- **A window shorter than 20 minutes.** The owner cache keeps an entry 600 seconds
  (`OWNER_CACHE_TTL`, `src/run.rs:82`), so after an undo a stale revision can hold `observed` back
  for up to 10 minutes; a window of 10 would then give false `not-observed` results.
- **Closings for any Incident in the process's table, matched on first-seen time alone.** A new
  term's event-born occurrence can carry a first-seen time from before the term, the time of its
  Event's last occurrence, and match an Incident of an earlier term. Only the current term's
  Incidents get closings.
- **The memory limit and the container name kept as strings** in the table. Up to 72 heap bytes
  more an entry, and the result is the same: a keyed hash tells "differs as written" as well.
- **A disclaimer line in every text** ("kausa does not say the failure is over"). The messages'
  fixed words already say what was observed and nothing more, and a banned-word test keeps them
  so; a line that repeats it in every message is noise.

## Decision

Proposed to FedeAM on 2026-11-24; not accepted yet.

### 1. Closed

An incident is **closed** at the tick where `Tracker::tick` drops its key from `pending`
(`src/incident.rs:383-387`), when three things hold:

- the entry was emitted (`Pending.emitted`, `src/incident.rs:281-290`);
- 0028's table holds an Incident that kausa created **in the current lease term**, with the same
  namespace, owner kind, owner name and detection as the key (`IncidentKey`,
  `src/incident.rs:33-38`) and the same `firstSeenUnix` as the entry's `first_seen`, and whose
  closing has not been taken; each table entry carries the number of the term that created it;
- the detection is not `D008`.

`tick` returns those keys with their `first_seen` and `last_signal_at`, which `Pending` already
holds. Nothing else closes an incident: not the dedup window, not the sweep, not the deletion of
the Incident. At most one closing per Incident: the table entry marks it.

| Detection | `basis` | Closes when | Earliest after the last signal |
|---|---|---|---|
| `D001` to `D005` | `state` | No tick counted the state for more than 330 s | 330 s, plus up to one 5 s tick |
| `D014` | `state` | The Deployment's record was dropped because the rule stopped matching (progress, a new rollout, a pause, the deadline removed, the Deployment deleted), then 330 s | 330 s, plus up to 5 s |
| `D009` | `recent-state` | No Pod of the owner failed by eviction in 10 minutes, then 330 s | 600 + 330 = 930 s, plus up to 5 s |
| `D006`, `D007`, `D010` to `D013` | `event` | The first tick with no fresh matching Event: its last occurrence 10 minutes old or more, the Event gone, or the Pod it is about gone | at most 600 s + 5 s |
| `D008` | none | No closing: a failed Job is a terminal fact | n/a |
| `D014` handled (yields to another incident) | none | No Incident, so no match (`src/incident.rs:424-430`) | n/a |

The tick is 5 seconds (`TICK`, `src/run.rs:75`). **How it closed:** `source-lost` when a watch
that covers the Incident's namespace dropped its records on a `403` at a time after
`last_signal_at` (`on_watch_error`, `src/run.rs:1603-1624`, which drops them only for `forbidden`
errors); otherwise `stopped-observing`. The watch is the Pods watch for `D001` to `D005` and
`D009`, the Deployments watch for `D014` and the Events watch for the `event` basis. kausa keeps,
for the term, the time of the last such drop per kind and per watch, in a fixed array indexed by
the watch's position: 3 kinds × (1 + n) watches at most, for n listed namespaces.

**The gate of 0028's point 1 against this match.** 0028 creates at most one Incident per
fingerprint per `--dedup-hours`, and a second Incident is possible as it states. So:

- **A reoccurrence held back by the gate** is a new pending entry under the same key, with a new
  `first_seen`; no Incident has that `firstSeenUnix`, so its end is silent, as its `detected`
  message is (0027's point 5).
- **A second Incident** (the table dropped the first one's entry after 6 hours, or `--dedup-hours`
  is `0`) has its own entry and its own `firstSeenUnix`, and matches it.
- **Two entries can share the match.** `IncidentKey` has no fingerprint, so with `--dedup-hours`
  at `0` two forged Events can make two Incidents with the same key and the same `firstSeenUnix`
  under two fingerprints. The match then takes the one whose closing has not been taken, and the
  next closing of that key takes the other; no Incident is closed twice.
- **An entry dropped after 6 hours** under 0028's rule loses its closing, and the loss is counted
  (point 7).

### 2. The outcome of an accepted action

There is an outcome check only for an action that passed every check of 0028's point 6 and that
changes a workload: `rollout-undo`, `raise-memory-limit` or `set-image-pull-policy`.

- **Accepted** means the Remediation passed every check of 0028's point 6, whether its
  `remediation` message was then sent, capped or dropped. The first accepted record of an
  Incident is kept, `none` included; a later one, which only a delete and a new create of the
  fixed name can give, does not replace it and starts no second check.
- **When it runs:** with `incidents.reportEnd` and `incidents.reportRemediations` both on, for an
  Incident in the table whose Remediation is accepted in the current term.
- **What decides it:** the action id chooses the check. `proposedAt` is only reported. No other
  field of the Remediation is read, and none of its parameters.

**The Pods of the owner** are the Pods kausa holds whose resolved top-level owner has the
Incident's `object.uid`, the filter of `pods_of` (`src/run.rs:1744-1749`). An owner with no UID
gives `not-checkable`. **Non-terminal:** `phase` is neither `Failed` nor `Succeeded`.

The baseline is taken at acceptance, at `t_a` on kausa's monotonic clock, from those Pods:

| Detection | Action | Baseline `B` | Condition `C` at a tick |
|---|---|---|---|
| `D001` to `D005`, `D014` | `rollout-undo` | The revisions of the controller ReplicaSets of the non-terminal Pods, read through the owner cache (0028's point 3); at most 4 | At least one non-terminal Pod, and **every** one has a controller ReplicaSet with a known revision not in `B` |
| `D003` | `raise-memory-limit` | A keyed SipHash (a random key per process) of the `memory_limit` string of the container named by the fact statement, equal in every non-terminal Pod that has that container | At least one non-terminal Pod with that container, and **every** one has a limit whose hash differs: the limit "differs as written" |
| `D002` | `set-image-pull-policy` | That container's policy is `Never` in every non-terminal Pod that has it | At least one such Pod, and **every** one has `Always` or `IfNotPresent` |
| any other pair | none | none | No outcome: `none`, `needs-human` and `inspect` act on nothing, and 0028 allows `delete-evicted-pods` for no detection |

`memory_limit` is the Pod spec's `resources.limits` quantity, kept as written, at most 32 bytes
(`MAX_QUANTITY_BYTES`, `src/record.rs:39`; read at `src/record.rs:627`), and `image_pull_policy` is
`None` for any value but the three known ones (`src/record.rs:489-490`).

**The results**, a closed vocabulary that only kausa writes:

- `observed`: at a tick within the window, `C` held. It does not say who made the change, that the
  change caused anything, or that it stayed.
- `not-observed`: the window ended with at least one non-terminal Pod of the owner held, and `C`
  never held. It does not say that nobody acted, or that the failure goes on.
- `no-pods`: the window ended with no non-terminal Pod of the owner held. It does not say the owner
  was deleted.
- `not-checkable`: at acceptance there was no single baseline: no Pod, no such container, values
  that differ between Pods, more than 4 revisions, a revision kausa could not read, a policy other
  than `Never`, or an owner with no UID. Decided at once.
- `source-lost`: the Pods watch that covers the namespace dropped its records on a `403` after
  `t_a`. Decided at once.

**The window** is `[t_a, t_a + W]`, and the check ends at the first tick where `C` holds. `W` is
`incidents.outcomeWindowMinutes`, **20 to 360, 60 by default**.

### 3. Where it is recorded

`Incident.status` stays declared, empty and reserved, and `incidents/status` gets no verb. kausa
writes nothing new to the cluster and reads nothing new. The record of an end is:

- the log lines `incident closed` and `incident outcome`, through `log.rs`, at `info`, with the
  fields of the message;
- the metrics of point 6;
- with notifications on, the messages of point 4.

### 4. The messages

Two values of `type` within `v2`, under 0027's acceptance `Update`, its item "Within `v2` a value
of `type` may be added": a receiver that knows only `detected`, `explained`, `capped` and
`remediation` ignores them. Their common fields are those of `detected`: `schema`, `type`,
`kausa_version`, `detection`, `object`, `fingerprint` and `first_seen`, all from kausa's own memory
of the Incident, never read back.

```json
{"schema": "kausa.fedeam.com/notification/v2", "type": "closed", "kausa_version": "0.9.0",
 "detection": {"id": "D003", "name": "out of memory"},
 "object": {"apiVersion": "apps/v1", "kind": "Deployment", "namespace": "shop", "name": "web"},
 "fingerprint": "5d0c2b7e9a1f4c38", "first_seen": "2026-11-24T09:30:05Z",
 "closed": {"how": "stopped-observing", "basis": "state",
            "last_seen": "2026-11-24T09:58:42Z", "at": "2026-11-24T10:04:12Z", "pods_now": 3},
 "remediation": {"recorded": "yes", "action": "raise-memory-limit", "result": "proposed",
                 "time": "2026-11-24T09:41:20Z"}}
```

```json
{"schema": "kausa.fedeam.com/notification/v2", "type": "outcome", "kausa_version": "0.9.0",
 "detection": {"id": "D003", "name": "out of memory"},
 "object": {"apiVersion": "apps/v1", "kind": "Deployment", "namespace": "shop", "name": "web"},
 "fingerprint": "5d0c2b7e9a1f4c38", "first_seen": "2026-11-24T09:30:05Z",
 "outcome": {"action": "raise-memory-limit", "result_of_record": "proposed",
             "time": "2026-11-24T09:41:20Z", "condition": "memory-limit-changed",
             "window_seconds": 3600, "result": "observed", "ended_at": "2026-11-24T09:55:40Z"}}
```

(invented values; the version is whichever release ships this.)

| Field | Values |
|---|---|
| `closed.how` | `stopped-observing` or `source-lost` |
| `closed.basis` | `state`, `recent-state` or `event` |
| `closed.last_seen` | the `last_signal_at` of the entry: the last tick that counted a signal |
| `closed.at` | the closing tick |
| `closed.pods_now` | 0 to 1,000,000: the Pods of the owner kausa holds at the closing tick |
| `remediation.recorded` | `yes`; `no` (the report is on and nothing was accepted); `not-read` (the report is off); `source-missing` (0028's gauge of the `remediations` watch is 0) |
| `remediation.action` | only with `yes`: `none`, `needs-human`, `inspect`, `rollout-undo`, `raise-memory-limit` or `set-image-pull-policy` |
| `remediation.result`, `outcome.result_of_record` | `proposed`; `executed` may be added later, as a value |
| `outcome.action` | `rollout-undo`, `raise-memory-limit` or `set-image-pull-policy` |
| `outcome.condition` | `revision-changed`, `memory-limit-changed` or `pull-policy-not-never`, fixed by the action |
| `outcome.window_seconds` | 1,200 to 21,600 |
| `outcome.result` | the five results of point 2 |
| `outcome.ended_at` | the tick at which `C` held or the window ended |
| every time | `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$`, 20 characters |

- **Nothing else.** No container name, and nothing written by kurita but the action id and
  `proposedAt` that 0028's checks already let out.
- **The text formats.** `slack` and `telegram` get one fixed line per combination, pinned by golden
  tests and fixed by the reference page, such as `kausa: stopped seeing out of memory (D003) in
  shop, Deployment web at 10:04:12 UTC (last seen 09:58:42; 3 Pods held). kurita proposed
  raise-memory-limit at 09:41:20.` The `event` basis says "no fresh Warning event for … in 10
  minutes"; `rollout-undo` says "the ReplicaSet revision of every running Pod changed", never "the
  undo was applied".
- **Banned words.** A test checks that no enum value and no fixed text holds `healthy`, `fixed`,
  `resolved`, `recovered`, `ok`, `success` or `worked`.
- **At most one of each per Incident**, each queued after its log line, in either order.
- **One bound for both, their own.** At most the value of `--notify-max-per-hour` of `closed` and
  `outcome` messages together start in any 60 minutes, counted apart from the `detected` and the
  `remediation` messages; no new flag. Over it a message is not sent, counted as `capped` and
  logged as `incident end capped` at most once an hour.
- **In the queue they rank with `remediation`.** On the way in, when the queue of 16 is full, the
  oldest queued message of the three lower types (`remediation`, `closed`, `outcome`) is dropped
  first; with none queued, a new message of those types drops itself; only then does 0027's rule
  apply (`src/notify.rs:478-494`). On the way out, after the owed `capped` message when it is due,
  `take_next` takes the oldest message that is neither of the three, and the oldest of the three
  only when no `detected` and no `explained` message is queued. **Among the three, arrival
  order:** the queue is one `VecDeque` taken with `pop_front` (`src/notify.rs:524-566`), and
  nothing in its shape asks for another order. So a `detected` or an `explained` message waits for
  an end message only when one is being sent as it arrives, at most 10 + 30 + 10 = 50 seconds, as
  0028's point 6 computes for `remediation` (`TIMEOUT`, `src/notify.rs:57`; `RETRY_WAIT`,
  `src/notify.rs:70`). A dropped end message is counted and logged as `incident end dropped` at
  most once an hour.
- **Neither owes the `capped` message**, for 0028's reason: its text speaks of incidents
  (`src/notification.rs:170-175`).
- **The `sent` check of `detected` messages does not apply to them** (`src/notify.rs:474`, `:539`):
  each is its own kind of item, and the table entry's marks are its only dedup. A test shows it: a
  `closed` message for a fingerprint whose `detected` message was sent is sent.
- **In any 60 minutes at the default of 30**, the webhook carries at most 30 `detected` + 30
  `remediation` + 30 end messages + 1 `capped`, plus the `explained` messages that the call budget
  bounds (30 by default): 30 + 30 + 30 + 1 + 30 = 121, 0028's 91 plus this ADR's 30.

### 5. Permissions

No change, with the switches on or off: the rendered roles are 0028's verb for verb, and
`incidents/status` keeps no verb. No new `get`, `list` or `watch`: the checks read the existing
Pod watch and the existing cached metadata `get` of `replicasets` of the owner lookup.

### 6. Logs and metrics

The log lines of point 3, and `incident end capped`, `incident end dropped` and `incident end
lost`, the last three at most once an hour. Three counters, proposed here; the reference page fixes
the names with the code:

- `kausa_incident_ends_total{kind, result}`: what kausa observed. `kind="closed"` with `result` the
  `how` of point 1; `kind="outcome"` with `result` the five results of point 2. Seven series.
- `kausa_end_notifications_total{type, outcome}`: the messages, `type` `closed` or `outcome`, and
  `outcome` `sent`, `failed`, `dropped` or `capped`, the shape of 0028's
  `kausa_remediation_notifications_total{outcome}`. Eight series.
- `kausa_incident_ends_lost_total{kind}`: closings and checks dropped before they happened
  (point 7). Two series.

All series start at 0 and are shown from the start. `kausa_notifications_total` never counts an
end message, in any outcome, so its `capped` and `dropped` keep their meaning
([Alerts](../reference/alerts.md)); `kausa_notifications_retried_total` does not count them either.
The queue cleared at the end of a term (`src/run.rs:947-953`) counts its end messages as `dropped`
in their own counter, not in `kausa_notifications_total`.

### 7. Switches, restart and failover

| Chart value | Flag | Default | Needs | Effect |
|---|---|---|---|---|
| `incidents.reportEnd` | `--report-end` | `false` | `incidents.enabled`, `dryRun: false` | closings: log line, metrics and, with notifications on, `closed` messages |
| `incidents.outcomeWindowMinutes` | `--outcome-window-minutes` | `60` (20 to 360) | `reportEnd` and `incidents.reportRemediations` | outcome checks and `outcome` messages |

With every switch at its default nothing changes: no new watch, verb, write, table field or
message. kausa refuses to start with `--report-end` and `--dry-run`, as with incidents.

Closings exist only for the Incidents of the current term, and checks live in it (the tracker is
per term: `lead`, `src/run.rs:959-964`):

- **At the end of a term** (the lease lost, `SIGTERM`), the closings not yet taken and the pending
  checks are counted in `kausa_incident_ends_lost_total` and dropped.
- **After a crash** nothing is counted.
- **Nothing is read back:** no status, no list of Incidents. The standby holds nothing and sends
  nothing.
- **A table entry older than 6 hours** dropped under 0028's rule loses its closing or its pending
  check, and the loss is counted.

So a missing `closed` message never means the failure is still there.

### 8. What the two messages say, and never say

- **`closed`** says: kausa stopped counting this detection's signal for this Incident, how
  (`stopped-observing`, or `source-lost` when it lost the permission to see it), on which basis
  and when; how many Pods of the owner it held then; and what kurita recorded for it, if anything.
- **`outcome`** says: whether the typed change that the accepted action makes was seen on every
  non-terminal Pod kausa holds of the owner, within a stated window.
- **Neither says** that the workload is healthy, fixed or resolved, that the failure will not come
  back, that kurita or anyone acted, or that the change caused anything. In kurita v0.1 nothing
  executes: an `observed` is always a change someone else made. A later ADR of kurita's that
  executes adds `executed` as a value of `result_of_record`, without a schema break.

### 9. Invariants

Changed in [ARCHITECTURE.md](../../ARCHITECTURE.md) with the code:

- **Invariant 11** gains the `closed` and `outcome` messages and their fields.
- **Invariant 12** gains: "The end of an incident is stated only in the closed vocabulary of
  0029; changing it needs an ADR."
- **Invariant 5's exception** for a Remediation's action id and `proposedAt` also covers choosing
  the outcome check and reporting its time; nothing else read from a Remediation is used.

### Trust statement

0028's statement holds whole. This ADR adds no writer, no verb and no record: **a writer of
`incidents/status` changes nothing kausa sends**, since kausa neither writes nor reads `status`.
What each principal gains:

- **A writer of Remediations in N** (kurita's service account, or whoever holds a wildcard grant):
  it chooses which outcome check runs for a real Incident kausa created in N, among the actions
  0028 allows for its detection, and when the window starts, by when it writes the record; it puts
  its action id and time into the `closed` message; and it causes one `outcome` message per
  Incident, within the end bound. It never chooses the result, never reaches another namespace
  (0028's lookup by namespace, name and UID) or an Incident of an earlier process, and never costs a
  `detected` or an `explained` message. Worst case: it records `rollout-undo` and rolls the
  Deployment itself; kausa reports `observed`, which is true of the Pods, and names kurita only as
  the proposer of record.
- **Whoever can create Events in N** (the built-in `edit` role can, and can delete them): one
  `closed` message per Incident a forged Event makes kausa create (0028's trust statement), within
  the end bound; it can delay a closing by refreshing the Event and bring it forward by deleting
  it. No acting id comes from an Event alone, so it never gets an `outcome`.
- **Whoever can create or delete Pods in N** (`edit` can): a failing forged Pod keeps a state
  incident open, since owner resolution goes by the ReplicaSet's name, which delays its `closed`
  but not the end of its outcome window; it can hold a check at `not-observed`, or make it
  `not-checkable` at acceptance with values that differ; it reaches `observed` only when every
  real non-terminal Pod already shows the change, or when there is none, where one forged Pod
  gives `observed` instead of `no-pods`; and deleting the Pods closes a state incident with
  `pods_now: 0`, which is true. `edit` can also change the Deployment for real, so the forgery
  buys little.
- **Unchanged:** kausa executes nothing, changes no workload, reads no Secret or ConfigMap, and
  sends nothing new to the model.

### Costs, computed

- **Writes:** 0. **New reads, watches and verbs:** 0. **The fact queue:** +0 bytes: the baseline is
  taken at acceptance, and the container name is already in the fact statement.
- **Each entry of 0028's table, with `reportEnd` on:** +32 bytes, counted even where 0028 already
  keeps one of the values: the term number (`u32`), the accepted action (`u8`), the flags (`u8`:
  closing taken, outcome taken, a record accepted) and padding, 8; `proposedAt` in Unix seconds
  (`i64`), 8; the keyed hash of the container's name (`u64`), 8; the pending check, an
  `Option<Box<Check>>`, 8. 500 × 32 = 16,000 bytes.
- **A pending check:** `t_a` (an `Instant`, 16 bytes), the baseline (4 × `u64`, a length and a tag,
  padded: 40) and the action (1): 16 + 40 + 1 = 57 bytes, padded to 64, at most 80 with the
  allocator's header and rounding. At most one per Incident, so 500 × 80 = 40,000 bytes.
- **The table, at 500 entries:** 383,000 bytes (0028's estimate) + 16,000 + 40,000 = 439,000
  bytes, 439,000 / 1,048,576 = 0.419 MiB, under 0028's bar 3 of 0.5 MiB = 524,288 bytes by
  524,288 − 439,000 = 85,288 bytes. The bar is not raised.
- **The drop times of point 1:** a fixed array of `Option<Instant>`, 16 bytes each on a 64-bit
  target, 3 × (1 + n) entries: at 100 listed namespaces, 3 × 101 = 303 entries, 303 × 16 = 4,848
  bytes. Keyed by `WatchKey`, which is an `Option<String>` (`src/run.rs:145`), a map would add
  each key's 24-byte header and a heap copy of the namespace's name of up to 63 bytes, plus the
  map's own overhead, about ten times as much, so the array it is.
- **This ADR's share:** 16,000 + 40,000 + 4,848 = 60,848 bytes, under 64 KiB = 65,536 bytes by
  4,688 bytes (bar 4).
- **CPU:** with at least one check pending, one pass over the Pods kausa holds at each 5-second
  tick, with one map lookup per Pod: 10,000 lookups a tick at 10,000 Pods. Hashing applies only to
  the containers of Pods with a check. Not measured (bar 5).
- **Messages:** at most 2 per Incident, and at most 30 end messages an hour at the default.

### Conditions of acceptance

Measured in kind with 10,000 Pods and on three Raspberry Pi 4 Model B (8 GB, k3s, about 100 Pods),
before this ADR is accepted, and after 0028's bars:

1. **Every switch off:** idle memory within the run-to-run spread of the previous release; the
   `kubectl auth can-i` matrix equal to 0028's; the logs, Events and messages of the scenario
   byte-identical to the previous release's.
2. **The switches on:** the `can-i` matrix still equal to 0028's.
3. **A full fact queue** at the longest names adds at most 2 MiB (0027's point 10), measured again
   with the switches on.
4. **0028's table at 500 entries,** each at the longest values with a pending `rollout-undo` check,
   takes at most 0.5 MiB (0028's bar 3, not raised); this ADR's share, by difference with
   `reportEnd` off, at most 64 KiB, with the drop times at 100 listed namespaces.
5. **The tick in kind** with 10,000 Pods and 500 pending checks takes less than 1 second.
6. **500 Incidents closing within one minute**, in kind and on the Raspberry Pis: no watch error,
   relist or restart; the `closed` messages received, plus those counted `failed`, `capped` or
   `dropped`, equal the closings counted, nothing is counted lost, and no Incident is in two
   `closed` messages; the `detected` messages sent in that hour equal those of the same scenario
   with `reportEnd` off.
7. **One kind scenario per acting row,** with `W` at 20: `D001` undone by hand gives `observed`,
   with an `ended_at` at most 600 seconds plus the rollout's time after the undo, and left alone
   `not-observed`; `D003` raised by a rollout, and again by an in-place resize, gives `observed`;
   `D002` with its policy changed gives `observed`; `D014` undone gives `observed`; the owner
   scaled to 0 gives `no-pods`; `D009` closes with `recent-state` and no outcome; `D008` gives no
   `closed`; `D011` on a ReplicaSet whose Event stops gives `basis: event`.
8. **A revoked Pods permission** in a listed namespace closes its open state incidents with
   `source-lost`, and its pending checks end `source-lost`.
9. **Killing the leader** during an open incident with a pending check gives no `closed` and no
   `outcome` for that Incident, and a `closed` for the new Incident of the same fingerprint.
10. **31 acting Remediations within an hour,** written by a test principal, give 30 `outcome`
    messages and 1 counted `capped`, and change no `detected` count.
11. **The vocabulary test** passes on every enum value and fixed text.

### Not verified

Each was read in source or documentation only, or not derived; the bar or test that settles it:

- **That an undo gives the restored ReplicaSet a new, higher revision** (the Deployment
  controller's `sync.go` and `deployment_util.go`, read on `master` on 2026-11-24). Bar 7, rows
  `D001` and `D014`.
- **That an in-place resize writes the Pod spec's limit, as the watch sees it.** Bar 7, row `D003`.
- **How long the owner cache's stale revision delays `observed` after an undo,** at most 600
  seconds computed. Bar 7, row `D001`.
- **That the Events of `D011` to `D013` stop while the failure holds** (observations on kind in
  [Detection and redaction rules](../reference/rules.md); the cause a hypothesis). Bar 7, row
  `D011`.
- **That a `403` drop and its relist behave as `on_watch_error` says.** Bar 8.
- **How long the garbage collector takes to delete a Pod whose controller `ownerReference` names a
  UID that matches no object,** the window of a forged Pod. In kind, create one and time its
  deletion.
- **The layout of `Instant`, the baseline and the allocator's rounding,** behind the 32 and 80
  bytes, and 0028's 383,000 bytes on which the margin sits. Bar 4, and 0028's bar 3.
- **The tick's CPU with 500 pending checks at 10,000 Pods.** Bar 5.
- **The end bound and the queue rank,** new code: the queue ranks two kinds today. Unit tests, and
  bars 6 and 10.
- **Nothing above is measured.**

## Consequences

- **A receiver learns when kausa stopped seeing an incident**, and, after kurita proposed one of
  three actions, whether the change it makes was seen, without kausa writing, reading or
  executing anything more. Both are off by default; with them off nothing changes.
- **kausa still never says "resolved".** Its two new statements are about its own observation and
  about a typed field of the Pods; the banned-word test keeps them so.
- **A missing `closed` is not an open failure:** a restart, a failover, a table entry dropped
  after 6 hours, `D008`, or an incident held back by the gate all leave none, and only the first
  three are counted.
- **For `D011` to `D013` a `closed` can come with the failure still there,** because Kubernetes
  stopped updating the Event; its return inside `--dedup-hours` is silent, as today. Stated in the
  message (`basis: event`) and on the reference page.
- **A proposal nobody applied gives `not-observed`** after the window, 60 minutes by default.
- **A writer of Remediations gains one `outcome` message per Incident** and the choice of which
  check runs, never the result; a writer of Events gains one `closed` per forged Incident; neither
  costs a `detected` or an `explained` message.
- **kurita changes nothing.** Its ADR 0008 reads `Incident.status` as declared and empty, which it
  stays.
- **Pages that change with the implementation**, not with this ADR: [Data sent in
  notifications](../reference/data-sent-in-notifications.md) (the two messages, their fields and
  texts, and that a missing `closed` is not an open failure), [Detection and redaction
  rules](../reference/rules.md) (the closing of each detection, and the caveat for `D011` to
  `D013`), [Command line](../reference/command-line.md) (the flags, log lines and metrics),
  [Helm chart](../reference/helm-chart.md) (`incidents.reportEnd`,
  `incidents.outcomeWindowMinutes`), [Alerts](../reference/alerts.md),
  [Permissions](../reference/permissions.md) (unchanged, said), 0028's reference page for the
  Incident resource (`status` still reserved), [Send notifications to Slack, Telegram or a
  webhook](../how-to/send-notifications.md) (the two messages, their bound and their place in the
  queue), [ARCHITECTURE.md](../../ARCHITECTURE.md) (invariants 5, 11 and 12) and
  [CHANGELOG.md](../../CHANGELOG.md); the chart's `values.yaml` and its schema; and the kind test,
  `tests/e2e/run.sh`.
