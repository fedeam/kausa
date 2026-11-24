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
- Amends, when accepted, each with an `Update 2026-11-24` paragraph at its end: [0015. Post each new
  explanation to one optional webhook, as JSON, Slack or Telegram](0015-notifications.md) (two new
  kinds of message, and the queue sentence of 0028's `Update` there);
  [0026](0026-facts-before-the-model.md) (its point 3);
  [0027](0027-fact-statement-for-the-detections.md) (its point 1's `Pods now`, corrected with every
  switch off, its point 5, its tracker and its acceptance item on `capped`);
  [0028](0028-incident-resource-and-remediation-report.md) (its points 1, 3, 4, 5, 6 and 9, its
  "Alternatives considered", its trust statement, its Costs and its "Not verified"); and invariants
  5, 11 and 12 of [ARCHITECTURE.md](../../ARCHITECTURE.md), changed with the code
- Keeps: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md) (nothing new goes to the model) and [0009. Run with
  read-only permissions that never include Secrets](0009-least-privilege-rbac.md) (no permission
  changes, with the switches on or off)

kurita is a separate project, a controller that proposes a remediation for an incident kausa
reports; its ADR 0008, the contract with kausa, is the other half of 0028. This ADR closes the loop
the maintainer asked for: kausa detects and publishes the incident, kurita re-checks it and
proposes, and kausa then says, by its webhook, how the incident ended. **kausa cannot know that an
incident was resolved, and never says so.** It says two narrower things, each typed: that it
stopped seeing the failure, on which basis and when; and, after kurita proposed one of three actions
that change a workload, whether it saw the change that action makes on every non-terminal Pod of the
workload, Pending ones included, within a stated window. In kurita v0.1 nothing executes, so an
`observed` is always a change someone else made, by hand or otherwise. Every figure of memory, size
and time below is **computed, not measured**, unless it says measured.

## Context

Status as of 2026-11-24: `main` at `4b85369`, after the merge that implements and accepts 0027, and
v0.6.3 released; every path and line cited below is `main`'s. 0028 is `proposed`, with kurita's ADR
0008, "Contract with kausa", as its other half. Nothing below is implemented. kurita is not
published as of 2026-11-24, so its ADRs are cited by number and title, without links.

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

**What a review of this proposal changed in that judgment,** on 2026-11-24, after a security review
read the source: three departures, each with its reason. The closing's match uses a `first_seen`
that the tracker stops lowering once the entry is emitted, since `observe` lowered it at every later
signal (`src/incident.rs:452`) and a late Event with an earlier time made the closing match nothing,
uncounted; and a closing that matches nothing is counted (point 1). The Pods of the owner are those
of the Incident's namespace, since `pods_of` filters by UID alone over every watched namespace
(`src/run.rs:1744-1749`) and a Pod in another namespace could name the owner's UID (point 2). And in
the queue an end message never evicts a `remediation`, since whoever writes Events can cause
closings, while a `remediation` rests on a record only the two controllers write (point 4). None
raises a bar or adds a byte to the costs. The second one changes 0027's `Pods now` with every switch
off, and is made unconditional, as a correction (point 2).

**Two departures from the judgment's details,** each a correction. For `D014` the watch whose loss
gives `source-lost` is the Deployments watch, not the Pods watch: `D014`'s signal comes from the
Deployment's record, so a lost Pods watch does not end it (point 1). And `D009` closes at most 935
seconds after the last eviction, 600 of freshness, 330 of grace and up to 5 of tick, with no
minimum beyond the 330, where the judgment said 930 seconds at the earliest: deleting the evicted
Pods ends the signal at once (point 1's table).

The fixed limits of that brief, which this ADR keeps: kausa executes nothing and reads no Secret
and no ConfigMap; nothing new goes to the model, and nothing of the closing or the outcome comes
from it; no free text written by another controller leaves by the webhook; what leaves about a
remediation rests on a resource only the two controllers write and on live objects kausa reads
itself, never on an Event alone; with incidents off kausa behaves as before 0028, but for one
correction, of 0027's `Pods now` (point 2); the two projects couple only through cluster resources.

## Alternatives considered

- **Writing the end into `Incident.status`**, the slot 0028's point 9 reserved, with `patch` on
  `incidents/status`. Rejected; the slot stays declared, empty and reserved. The review read in the
  Kubernetes source (`kubernetes/kubernetes` at `master`, fetched on 2026-11-24, no commit recorded;
  the paths are under "Not verified") that the design as proposed does not hold: a merge patch to
  `/status` that carries `metadata.uid` is not refused when the UID differs, the UID is ignored (the
  patch handler passes no preconditions, and the status strategy copies the whole old object over
  the new one), so after a delete and a new create under the same name kausa would patch the status
  of an object it did not create; and a rule placed on `status` does not run when a merge patch
  removes `status` itself (`status: null`), so a written result could be erased. Both can be fixed
  (a `resourceVersion` precondition, a root rule `!has(oldSelf.status) || has(self.status)`), and
  kurita would have to parse only `metadata` and `spec`. What remains costs `patch` on
  `incidents/status`, two writes per Incident, one `MODIFIED` event per write to kurita's watch, and
  a way for any holder of `*/status` to write a result first and make kausa send nothing. What it
  buys is that `kubectl get incident` shows the end, for the day an Incident is kept; the webhook
  receiver, who asked, gets nothing from it. The rejection does not rest on those four readings: not
  writing `status` is the smaller design even if a kind test proves them wrong.
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
  messages. One shared bound for the two end messages instead (point 4).
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
  Then a record of `none` that passed kausa's checks would show as `recorded: no`, which says that
  kausa holds no such record. `none` is shown as `recorded: yes, action: none`.
- **A window shorter than 20 minutes.** The owner cache keeps an entry 600 seconds
  (`OWNER_CACHE_TTL`, `src/run.rs:82`), so after an undo a stale revision can hold `observed` back
  for up to 10 minutes and the wait for its read (point 2); a window of 10 would then give false
  `not-observed` results.
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
  closing has not been taken (the oldest such entry when two qualify); each table entry carries the
  number of the term that created it;
- the detection is not `D008`.

`tick` returns those keys with their `first_seen` and `last_signal_at`, which `Pending` already
holds. **What the match uses is frozen at emission:** `observe` stops lowering `first_seen` once
the entry is emitted, where today it lowers it at every signal (`entry.first_seen =
entry.first_seen.min(first)`, `src/incident.rs:452`), so a later signal of the same key with an
earlier time (another Pod of the owner, or an Event whose `lastTimestamp` its writer set earlier,
`src/incident.rs:175`) no longer moves it away from the Incident's `firstSeenUnix`. Nothing of
0027 changes: an emitted entry's `first_seen` is read by no one but this match, since `emit` runs
only for an entry not emitted (`src/incident.rs:397`, `:421`). No field is added. A test pins it: a
later Event of the same key with an earlier `lastTimestamp`, then the closing, which matches.

**A closing with no match** (an emitted entry dropped by `tick` whose key and `first_seen` match no
entry of the current term: an incident held back by the gate, refused while 500 Incidents are
held, a create that failed or is still in flight, a `D014` handled by another incident, or a table
entry dropped after 6 hours) sends nothing, and is counted under its own result, `no-incident`
(point 6). `D008` is not a closing and is not counted.

Nothing else closes an incident: not the dedup window, not the sweep, not the deletion of the
Incident. At most one closing per Incident: the table entry marks it.

| Detection | `basis` | Closes when | The closing tick comes |
|---|---|---|---|
| `D001` to `D005` | `state` | No tick counted the state for more than 330 s | 330 to 335 s after the last signal, the last tick that counted the state |
| `D014` | `state` | The Deployment's record was dropped because the rule stopped matching (progress, a new rollout, a pause, the deadline removed, the Deployment deleted), then 330 s | 330 to 335 s after the last signal |
| `D009` | `recent-state` | No Pod of the owner held that failed by eviction in the last 10 minutes, then 330 s | 330 to 335 s after the last signal; from the last eviction at most 600 + 335 = 935 s, and no minimum beyond the 330: deleting the evicted Pods ends the signal at once |
| `D006`, `D007`, `D010` to `D013` | `event` | The first tick with no fresh matching Event: its last occurrence 10 minutes old or more, the Event gone, or the Pod it is about gone | At most 600 + 5 = 605 s after the Event's last occurrence; at the next tick, within 5 s, when the Event or the Pod is gone |
| `D008` | none | No closing: a failed Job is a terminal fact | n/a |
| `D014` handled (yields to another incident) | none | No Incident, so no match: counted `no-incident` (`src/incident.rs:424-430`) | n/a |

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
  `first_seen`; no Incident has that `firstSeenUnix`, so its end sends nothing, as its `detected`
  message is not sent (0027's point 5), and is counted `no-incident`.
- **A second Incident** (the table dropped the first one's entry after 6 hours, or `--dedup-hours`
  is `0`) has its own entry and its own `firstSeenUnix`, and matches it.
- **Two entries can share the match,** at any `--dedup-hours`. `IncidentKey` has no fingerprint,
  and the tracker holds one entry per key at a time, so two Incidents share a key and a
  `firstSeenUnix` only when two entries of an event key, one after the other, got the same
  `first_seen`: whoever writes the Events chooses their `lastTimestamp`, and a different note
  gives a different fingerprint, which the gate does not hold back. The match takes the oldest
  whose closing has not been taken; when the first entry's closing came before its create landed
  (counted `no-incident`), the second entry's closing takes the first Incident and names its
  fingerprint. No Incident is closed twice. Freezing `first_seen` at emission does not change this:
  each entry is frozen at its own emission.
- **An entry dropped after 6 hours** under 0028's rule loses its pending check, counted lost (point
  7); its closing, when the tracker drops the key, is counted `no-incident`.

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

**The Pods of the owner** are the Pods kausa holds **in the Incident's namespace** whose resolved
top-level owner has the Incident's `object.uid`. This changes `pods_of` (`src/run.rs:1744-1749`),
which today filters by UID alone over the Pods of every watched namespace: `top_owner` keeps a
controller `ownerReference` that is neither a ReplicaSet nor a Job as the Pod carries it, UID
included (`src/owner.rs:61-63`), and so does a lookup that finds no controller or fails
(`:66-67`), so a Pod in another namespace could name the owner's UID. The same set gives
`closed.pods_now`. An owner with no UID gives `not-checkable`. **Non-terminal:** `phase` is
neither `Failed` nor `Succeeded`; Pending Pods count.

**The correction of 0027's `Pods now`, unconditional.** `pods_of` also gives 0027's `Pods now`
(`src/run.rs:1208`, `:1692`), which counts the same way today, so a Pod of another namespace that
names the owner's UID inflates it. The change applies with every switch off, not only with
`incidents.reportEnd`: keeping a count known to be wrong as the default is worse than changing it.
It is the one thing this ADR changes with every switch off: `Pods now` counts only the Pods of the
owner's namespace, in the fact statement, the request to the model (`pods_total`,
`src/context.rs:120`), the `pods_total` field of the `incident` log line (`log_incident`, called at
`src/run.rs:1213`, the field at `:1384`), the `Detected` Event, the webhook's `facts.podsNow` and
0028's `facts.podsNow`; no new watch, verb, write, table field or message. It changes the count only
for an owner whose UID a Pod of another namespace names, and it can ship ahead of the rest of this
ADR, in its own change, since it depends on nothing else here.

**Where a Pod's revision comes from.** As 0028 decides it (its point 3, "Where the revision comes
from"): a ReplicaSet's revision, with the UID its `get` returned, is held in the owner cache's
entry, which only the owner lookup (`controller_of`, `src/run.rs:1909-1953`) fills, from async code.
0028's reader is the fact slot; this ADR adds a second one, since nothing else reads a ReplicaSet
again while its Pods live (`on_pod` reuses a resolved owner, `src/run.rs:1448-1452`). How it reads,
and the bound below, are the ADR author's design, written after a third review found that lookups
awaited in the run loop (`src/run.rs:1007-1235`), as `on_pod`'s are (`:1462-1463`, awaited from the
`select!` at `:1090`), would hold it, with no tick, no watch event and no fact slot served, for as
long as the `get`s of every pending check take: 1,000 in a row every 600 seconds at 500 checks, or 5
seconds each with an API server that does not answer. So:

- **The run loop never waits for a revision.** The reads run in a read slot, a future polled as one
  more branch of the run loop's `select!`, as 0027's fact slot is (`reporting`, declared at
  `src/run.rs:1005`, assigned at `:1012` and polled at `:1108`), with one `get` in flight at a time.
  It borrows the term's lookup (`:973`) as 0028's read does; the cache's `RefCell` (`:1905`) is
  never borrowed across an `await`, so the read slot, the fact slot, `on_pod` and the run loop share
  it. `C` is evaluated right after each tick, in the run loop, from the cache alone, with no
  request: a Pod's revision is known when its controller ReplicaSet's entry is fresh, its `get`
  answered with a whole-number revision, and the UID that `get` returned equals the one in the Pod's
  `ownerReference`.
- **At most 16 `get`s started per tick, and never faster than one every 312.5 ms.** At each tick the
  loop lists the names that the pending `rollout-undo` checks need read, at most 16: a ReplicaSet
  named by the controller reference of a non-terminal Pod of a check's set, whose entry is absent,
  expired, or a failure older than 30 seconds (`OWNER_ERROR_TTL`, `:83`). It takes one name per
  check per round, that check's next one, the checks in the order of 0028's table, and the slot
  reads them in that order. The list starts at the first name the slot has not read: the slot keeps
  the table index of the check after the last one whose read ended (answered, failed or timed out),
  8 bytes, and the next tick's list, which replaces this one, starts there, so a name the slot did
  not reach is first in the next list. With N checks that need a read and r reads ended a tick,
  every check gets its read within ⌈N / r⌉ ticks of its previous one, at any r ≥ 1: none starves.
  The slot starts a `get` only 312.5 ms (5 / 16 seconds) or more after it started the previous one,
  keeping that start's time, 16 bytes. The tick keeps tokio's default behavior for missed ticks
  (`Burst`: `tokio::time::interval` at `src/run.rs:983`, polled at `:1196`, with no
  `set_missed_tick_behavior`), so after a handler held the run loop late ticks fire in a row, each
  with its list; the spacing makes the bound one of time, not of ticks: kausa sends at most 16 of
  these `get`s in any 5 seconds, 3.2 a second, 11,520 in any hour, whatever the checks name and the
  ticks do. Changing the tick to `Delay` or `Skip` was not taken: it would change the timing of the
  tick's other work with every switch off. The spacing and the list's start are the ADR author's
  design, added after a fourth review; with an API server that does not answer, each holds the slot
  5 seconds (`OWNER_LOOKUP_TIMEOUT`, `:89`), about one a tick, 0.2 a second. Each is the owner
  lookup's metadata `get` of `replicasets`, and the forced read below is the same call past a fresh
  entry: no new verb and no new watch.
- **At acceptance, a forced read.** A `rollout-undo` check whose set names more than 4 distinct
  ReplicaSets is `not-checkable` at once, with no read, since two ReplicaSets of one Deployment hold
  different revisions ("Not verified"). Otherwise, in its turn, its names, at most 4, are listed
  together or wait together for a later tick, and each is read with a `get` even when its entry is
  fresh, within the 16; `B` comes from those answers, and `t_a` is the first tick after the last of
  them. A read that fails, or answers with another UID than the Pod's `ownerReference` or with no
  whole-number revision, makes the check `not-checkable`. A name the owner lookup refuses (0028's
  point 3: not a DNS-1123 subdomain of at most 253 characters) is never listed and is a read that
  fails, with no request. A check whose forced reads have not all ended 900 seconds after its
  acceptance is `not-checkable` then, with no `t_a`: 900 is above the 625 seconds the baselines of
  500 checks of 4 ReplicaSets take with every `get` answered within 312.5 ms (below). This deadline
  is the ADR author's design, added after the fourth review. So `B` is as of a read made after kausa
  accepted the record, never a cached value up to 600 seconds older: a rollback made before that
  read is in `B`, and an `observed` is a change seen after `t_a`.
- **`C` decided, or not.** At a tick `C` holds when every non-terminal Pod's revision is known and
  not in `B`. It is decided false when no Pod is non-terminal, when one has a controller reference
  that is not a ReplicaSet, or when one's known revision is in `B`; otherwise it is not decided,
  since a Pod's revision is not known. At the window's end, `not-observed` needs `C` decided at a
  tick of the window's last 600 seconds, the age an entry may have anyway; without one, the result
  is `not-checkable`. So a check whose reads fail or wait is never reported `observed` or
  `not-observed` on a revision kausa did not read.
- **What it costs at 500 checks** of 2 ReplicaSets each, accepted in one tick (a writer of
  Remediations can write their records together): 1,000 forced reads, at 16 a tick the last baseline
  taken 63 ticks, 315 seconds, after acceptance; then 1,000 entries refreshed every 600 seconds, 1.7
  `get`s a second, under the 3.2. A round lists one name of each check, or up to 4 of one that
  awaits its baseline, so a name waits for its read at most ⌈500 / 16⌉ = 32 ticks, 160 seconds, once
  the baselines are taken, and, while 500 checks await theirs, at most 1,000 / 16 = 63 ticks, 315
  seconds, with 2 ReplicaSets each, or 2,000 / 16 = 125 ticks, 625 seconds, with 4. After a change,
  `observed` comes at most an entry's 600 seconds plus that wait later: 605 seconds with one check
  of one ReplicaSet, 600 + 2 × 160 = 920 with 500 checks of 2, under the shortest window of 1,200
  seconds. With 500 checks of 4 it is 600 + 4 × 160 = 1,240 seconds, past that window by 40: a real
  change on a check of 4 ReplicaSets can end it `not-checkable` instead of `observed`, never
  `not-observed`, since its forced entries stop being fresh at `t_a` + 600 at the latest and the
  window's last 600 seconds begin at `t_a` + W − 600 ≥ `t_a` + 600. These waits hold with every
  `get` answered within 312.5 ms. With each answer held d seconds, the slot ends about 5 / d reads a
  tick instead of 16, and each wait is multiplied by about 16 d / 5: at d = 1, 500 checks of 2 have
  their baselines after 1,000 seconds, so those still waiting at 900 end `not-checkable`, and a
  round takes 500 seconds, so `C` is decided less often and more checks end `not-checkable`. No
  result is false: a check that waits ends `not-checkable`. The list holds at most 16 names of at
  most 253 bytes (0028's lookup rule), each with the index of its table entry, whose namespace it
  uses: 16 × (32 + 253) + 24 = 4,584 bytes, from one tick to the next. With a switch off, or with no
  `rollout-undo` check pending, nothing is listed and nothing is read.

The baseline is taken at acceptance from those Pods, at `t_a` on kausa's monotonic clock: the
acceptance, or for `rollout-undo` the first tick after its forced reads:

| Detection | Action | Baseline `B` | Condition `C` at a tick |
|---|---|---|---|
| `D001` to `D005`, `D014` | `rollout-undo` | The revisions of the controller ReplicaSets of the non-terminal Pods, from the forced reads after acceptance (above); at most 4 ReplicaSets | At least one non-terminal Pod, and **every** one has a controller ReplicaSet with a known revision not in `B` (decided or not, above) |
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
  never held; for `rollout-undo`, `C` was also decided at a tick of the window's last 600 seconds.
  It does not say that nobody acted, or that the failure goes on.
- `no-pods`: the window ended with no non-terminal Pod of the owner held. It does not say the owner
  was deleted.
- `not-checkable`: at acceptance there was no single baseline: no Pod, no such container, values
  that differ between Pods, more than 4 ReplicaSets or revisions, a revision kausa could not read, a
  policy other than `Never`, or an owner with no UID, decided at once (for `rollout-undo`, when its
  forced reads answer); or, for `rollout-undo`, its forced reads not all ended 900 seconds after
  acceptance, decided then, or `C` not decided at any tick of the window's last 600 seconds, decided
  at its end.
- `source-lost`: the Pods watch that covers the namespace dropped its records on a `403` after
  `t_a`. Decided at once.

**The window** is `[t_a, t_a + W]`, and the check ends at the first tick where `C` holds. `W` is
`incidents.outcomeWindowMinutes`, **20 to 360, 60 by default**.

### 3. Where it is recorded

`Incident.status` stays declared, empty and reserved, and `incidents/status` gets no verb. kausa
writes nothing new to the cluster and reads no new kind of object: point 2's reads are the owner
lookup's metadata `get` of `replicasets`, made more often, in a slot of their own and at most 16 a
tick. The record of an end is:

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
| `closed.pods_now` | 0 to 1,000,000: the Pods of the owner kausa holds at the closing tick (point 2's set); absent for `D012` and `D013`, whose owner, a PersistentVolumeClaim or a HorizontalPodAutoscaler, is no Pod's owner, so the count would always be 0 |
| `remediation.recorded` | `yes`; `no` (the report is on and kausa holds no record of kurita's for this Incident that passed the checks of 0028's point 6); `not-read` (the report is off); `source-missing` (0028's gauge of the `remediations` watch is 0) |
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
  raise-memory-limit at 09:41:20.` The `event` basis says "stopped seeing a fresh Warning event for
  …", with the two times the message carries, and never why: it is true whether the Event's last
  occurrence grew 10 minutes old, the Event was deleted or the Pod it is about was, and in the last
  two cases the closing comes at the next tick, seconds after `last_seen`; a golden test pins the
  same line for the three causes. `rollout-undo` says "the ReplicaSet revision of every
  non-terminal Pod changed", never "the undo was applied"; for `D012` and `D013` the line has no
  count of Pods.
- **Banned words.** A test checks that no enum value and no fixed text holds `healthy`, `fixed`,
  `resolved`, `recovered`, `ok`, `success`, `worked`, `applied`, `succeeded`, `remediated` or
  `restored`.
- **At most one of each per Incident**, each queued after its log line, in either order.
- **One bound for both, their own.** At most the value of `--notify-max-per-hour` of `closed` and
  `outcome` messages together start in any 60 minutes, counted apart from the `detected` and the
  `remediation` messages; no new flag. Over it a message is not sent, counted as `capped` and
  logged as `incident end capped` at most once an hour.
- **In the queue they rank below `remediation` on the way in, with it on the way out.** On the way
  in, when the queue of 16 is full, the oldest queued end message is dropped first, whatever
  arrives; with none queued, a new end message drops itself; only then does 0028's rule for
  `remediation` apply (the oldest queued `remediation` dropped, or a new one dropping itself), and
  then 0027's (`src/notify.rs:478-494`). So an end message never evicts a `remediation`: a closing
  can be caused by whoever writes Events, a `remediation` only by a record that the two controllers
  write. The rule is one more `position` search before the one the push already does
  (`src/notify.rs:483`). It qualifies the sentence of 0028's `Update` of 0015 that "in the queue a
  `remediation` never evicts another message": it evicts an end message. 0028's point 6, "a
  `remediation` never evicts a `detected` or an `explained` message", holds. Any message that
  arrives with the queue full evicts a queued end message first, a `detected` message included,
  since the `detected` bound applies when a message is taken, not when it is queued
  (`src/notify.rs:473-494`, `:543`). On the way out, after the owed `capped` message when it is due,
  `take_next` takes the oldest message that is neither of the three, and the oldest of the three
  only when no `detected` and no `explained` message is queued. **Among the three, arrival order:**
  the queue is one `VecDeque` taken with `pop_front` (`src/notify.rs:524-559`), and nothing in its
  shape asks for another order. So a `detected` or an `explained` message waits for an end message
  only when one is being sent as it arrives, at most 10 + 30 + 10 = 50 seconds, as 0028's point 6
  computes for `remediation` (`TIMEOUT`, `src/notify.rs:57`; `RETRY_WAIT`, `src/notify.rs:70`). A
  dropped end message is counted and logged as `incident end dropped` at most once an hour.
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
Pod watch and the existing cached metadata `get` of `replicasets` of the owner lookup, which a
pending `rollout-undo` check calls more often (point 2).

### 6. Logs and metrics

The log lines of point 3, and `incident end capped`, `incident end dropped` and `incident end
lost`, the last three at most once an hour. Three counters, proposed here; the reference page fixes
the names with the code, the label `outcome` beside `type="outcome"` included:

- `kausa_incident_ends_total{kind, result}`: what kausa observed. `kind="closed"` with `result` the
  `how` of point 1, or `no-incident` for a closing that matched no Incident; `kind="outcome"` with
  `result` the five results of point 2. 3 + 5 = 8 series.
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

With every switch at its default nothing changes but `Pods now`, which counts only the Pods of the
owner's namespace (point 2): no new watch, verb, write, table field or message. kausa refuses to
start with `--report-end` and `--dry-run`, as with incidents.

Closings exist only for the Incidents of the current term, and checks live in it (the tracker is
per term: `lead`, `src/run.rs:959-964`):

- **At the end of a term** (the lease lost, `SIGTERM`), the closings not yet taken and the pending
  checks are counted in `kausa_incident_ends_lost_total` and dropped.
- **After a crash** nothing is counted, a restart on memory during a relist included: every
  pending closing and check is lost, uncounted.
- **Nothing is read back:** no status, no list of Incidents. The standby holds nothing and sends
  nothing.
- **A table entry older than 6 hours** dropped under 0028's rule loses its pending check, counted
  in `kausa_incident_ends_lost_total`; its closing, when the tracker drops the key, matches nothing
  and is counted `no-incident`.

So a missing `closed` message never means the failure is still there.

### 8. What the two messages say, and never say

- **`closed`** says: kausa stopped counting this detection's signal for this Incident, how
  (`stopped-observing`, or `source-lost` when it lost the permission to see it), on which basis
  and when; how many Pods of the owner it held then; and what kurita recorded for it, if anything.
- **`outcome`** says: whether the typed change that the accepted action makes was seen on every
  non-terminal Pod kausa holds of the owner in its namespace, Pending ones included, within a
  stated window.
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
  it chooses which outcome check runs for a real Incident kausa created in N, among the actions 0028
  allows for its detection, and when the window starts, by when it writes the record; it puts its
  action id and time into the `closed` message; and it causes one `outcome` message per Incident,
  within the end bound, which is one for the process. It never chooses the result, never reaches
  another namespace (0028's lookup by namespace, name and UID) or an Incident of an earlier process,
  and never costs a `detected` or an `explained` message. Each `rollout-undo` it gets accepted costs
  at most 4 forced reads at acceptance and one read per round while pending, within the 16 a tick,
  and never holds the run loop; with a record for each of the 500 Incidents the table holds at most,
  the reads of other namespaces' checks wait at most 625 seconds while the baselines are taken (315
  with checks of 2 ReplicaSets) and 160 seconds a round after, with every `get` answered within
  312.5 ms (point 2). With checks of 4 ReplicaSets a change is seen up to 1,240 seconds later, so it
  can make a real undo on a check of another namespace with a window of 1,200 seconds end
  `not-checkable` instead of `observed`; with slower answers the waits grow and more such checks end
  `not-checkable`, a result lost, never a false one. Worst case: it records `rollout-undo` and rolls
  the Deployment itself; kausa reports `observed`, which is true of the Pods, and names kurita only
  as the proposer of record.
- **A writer of Incidents in N** (0028's trust statement): deleting an Incident and creating
  another under its name makes kurita's record for it fail kausa's check (`incident-unknown`,
  0028's point 6), so the closing says `recorded: no` and no outcome check runs. The closing itself
  comes from kausa's table, not from the object, and is still sent.
- **Whoever can create Events in N** (the built-in `edit` role can, and can delete them): in N, one
  `closed` message per Incident a forged Event makes kausa create (0028's trust statement), within
  the end bound; it can delay a closing by refreshing the Event and bring it forward by deleting it,
  and, since the match is frozen at emission, a later Event with an earlier `lastTimestamp` no
  longer suppresses one. Deleting the Event of a genuine event incident, or the Pod it is about,
  sends its `closed` at the next tick, seconds after `last_seen`, while the failure may go on: the
  message's times show it, and its text says only that kausa stopped seeing a fresh Warning event;
  if the failure comes back inside `--dedup-hours`, it is silent (the gate holds back its Incident,
  and 0027's point 5 its `detected` message). No acting id comes from an Event alone, so it never
  gets an `outcome`. Outside N, since the end bound and the queue are one for the process: 30 forged
  Incidents an hour, one Event with a new note every 3,600 / 30 = 120 seconds, within 0028's gate of
  500 in 6 hours (83 an hour), give 30 closings an hour that fill the end bound, and kausa then
  sends no `closed` and no `outcome` message about any namespace for that hour, each counted
  `capped`. With the queue full, a burst of its closings evicts end messages of other namespaces,
  counted `dropped`, never a `remediation`; so do the `detected` messages of its forged Incidents
  when several settle in one tick, since any message that arrives evicts a queued end message first
  (point 4), and once no end message is queued they evict a `remediation` by 0028's rule. How many
  `detected` messages fit in one tick is not measured. While the process lives, no end is lost
  uncounted. Within 0028's relist exposure it can make kausa restart on memory, and that loses every
  pending closing and check, uncounted (point 7).
- **Whoever can create or delete Pods in N** (`edit` can): a failing forged Pod keeps a state
  incident open, since owner resolution goes by the ReplicaSet's name, which delays its `closed` but
  not the end of its outcome window; it can hold a check at `not-observed`, or make it
  `not-checkable` at acceptance with values that differ; it reaches `observed` only when every real
  non-terminal Pod already shows the change, or when there is none, where one forged Pod gives
  `observed` instead of `no-pods`; and deleting the Pods closes a state incident with `pods_now: 0`,
  which is true. A Pod in N whose controller `ownerReference` names the Deployment with its real UID
  counts as one of its Pods (`top_owner` keeps such a reference as the Pod carries it,
  `src/owner.rs:61-63`), and the garbage collector does not delete it, since its owner exists: it
  lasts until someone deletes it. A Pod in another namespace counts for nothing in N (point 2).
  While a `rollout-undo` check is pending, a Pod it adds whose controller reference names a
  ReplicaSet kausa cannot read (none, or one with another UID: the lookup fails and `top_owner`
  keeps the reference, `src/owner.rs:67`) leaves `C` undecided while no other Pod decides it false,
  so the check ends `not-checkable`, never `observed`; each such name costs one `get` per round of
  its check, within the 16 a tick. A name that is not a DNS-1123 subdomain of at most 253 characters
  gets it the same result with nothing more: no request and no entry in the list or the cache
  (0028's lookup rule). Whether the garbage collector deletes such a Pod, whose owner does not
  exist, is under "Not verified". Outside N it gets nothing more: the run loop never waits for these
  reads, the 16 a tick hold whatever it adds, and a round lists one name of each check, so the
  checks of other namespaces wait at most 32 ticks, 160 seconds, for a read at 500 checks, as
  without it, with every `get` answered within 312.5 ms; slower answers are the API server's, or a
  writer of ReplicaSets' (below). `edit` can also change the Deployment for real, so the forgery
  buys little.
- **Whoever can write ReplicaSets in N** (`edit` can): point 2 reads the revision from the
  `deployment.kubernetes.io/revision` annotation of each Pod's controller ReplicaSet, so it can set
  that annotation on the Pods' real ReplicaSets to numbers not in `B` and make `C` hold with no Pod
  and no spec changed: a forged `observed`, whose text says that the ReplicaSet revision of every
  non-terminal Pod changed, true of the annotation it wrote and of no rollout. It can also hold `C`
  false by writing a number in `B`, or give `not-checkable` by deleting a ReplicaSet and creating
  another under its name, whose UID differs. The impact is low: the same role can roll the
  Deployment back for real, and kausa reports only what it read, naming kurita only as the proposer
  of record. Whether the Deployment controller writes the annotation back, and how soon, is under
  "Not verified". Outside N, its ReplicaSets are read only in its own checks' turns: a `get` it
  could make slow, which 0028's "Not verified" asks of a ReplicaSet with about 1.5 MiB of metadata,
  holds the read slot up to 5 seconds a read, never the run loop, and delays the reads of other
  namespaces' checks by that much per turn of its checks. If every read it causes takes 5 seconds,
  the slot ends about one read a tick, the waits of point 2 are multiplied by up to 16, and checks
  of other namespaces end `not-checkable` where they would have been decided, never with a false
  result.
- **Whoever can write `pods/status` in N** (a kubelet, for the Pods of its node, and principals with
  a wildcard grant: high-privilege principals): it can keep the status of a genuine state incident's
  Pods clean, against the kubelet, for more than 330 seconds, and so forge its `closed`; and it can
  mark real Pods `Failed`, taking them out of the non-terminal set, which gives `no-pods`,
  `not-checkable` at acceptance, or `observed` when at least one remaining Pod shows the change. A
  write to `pods/status` cannot make an `observed` without a real change on at least one Pod, as a
  writer of ReplicaSets can (above): the revision comes from the Pod's `ownerReference` and its
  ReplicaSet's annotation, the limit and the policy from the Pod's spec, none from its status; that
  a write to `pods/status` cannot change the Pod's `ownerReferences` or its spec is under "Not
  verified".
- **Unchanged:** kausa executes nothing, changes no workload, reads no Secret or ConfigMap, and
  sends nothing new to the model.

### Costs, computed

- **Writes:** 0. **New watches and verbs:** 0. **Reads:** with a `rollout-undo` check pending, the
  read slot's metadata `get`s of `replicasets` (point 2): at most 16 started a tick and 312.5 ms
  apart, one in flight, so at most 16 in any 5 seconds, 3.2 a second and 11,520 in any hour, of
  which at most 4 forced at each acceptance; at 500 checks with 2 ReplicaSets each, 1,000 at
  acceptance and then 1,000 every 600 seconds, 1.7 a second, before the cache shares entries. None
  with a switch off or with no such check pending, and the run loop never waits for one. Not
  measured (bar 5). **The fact queue:** +0 bytes: the baseline is taken at acceptance, and the
  container name is already in the fact statement.
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
- **The read slot of point 2:** at most 16 names of at most 253 bytes with their table index, 4,584
  bytes, the time of its last start, 16, and the index where the next list starts, 8: 4,608 bytes,
  held while a `rollout-undo` check is pending. Bar 4, which measures what the table keeps, does not
  count them; with them, this ADR's memory peaks at 60,848 + 4,608 = 65,456 bytes, 80 under 64 KiB,
  too close for a computed figure, so bar 5 records the slot's size apart. Apart from those bytes,
  the one `get` in flight decodes a ReplicaSet's metadata: an object near the API server's limit of
  about 1.5 MiB took about 12 MiB once parsed (0028's Costs, from 0014's `Update 2026-10-28`), an
  estimate for a metadata `get`, not measured. The read slot's decode can be alive together with
  `on_pod`'s lookup and 0028's fact slot read, one `get` in flight each: 3 × 12 = 36 MiB computed,
  18.75 % of the chart's memory limit of 192 MiB (`resources.limits.memory`,
  `charts/kausa/values.yaml:155`). The exposure exists today with one, in `on_pod`; 0028 adds the
  second and this ADR the third. Bar 5 measures it.
- **CPU:** with at least one check pending, one pass over the Pods kausa holds at each 5-second
  tick, with one map lookup and one comparison of the namespace per Pod: 10,000 of each a tick at
  10,000 Pods. Hashing applies only to the containers of Pods with a check. Not measured (bar 5).
- **The departures of Context** add no byte: the frozen `first_seen` is a condition in `observe`,
  the namespace a comparison in `pods_of`, the queue rule a search of the queue of 16. Point 2's
  revision reads keep nothing for the term either: the revision is in 0028's owner-cache entry.
- **Messages:** at most 2 per Incident, and at most 30 end messages an hour at the default.

### Conditions of acceptance

Measured in kind with 10,000 Pods and on three Raspberry Pi 4 Model B (8 GB, k3s, about 100 Pods),
before this ADR is accepted, and after 0028's bars:

1. **Every switch off:** idle memory within the run-to-run spread of the previous release; the
   `kubectl auth can-i` matrix equal to 0028's; the logs, Events and messages of the scenario
   byte-identical to the previous release's, except `Pods now` and the `incident` log line's
   `pods_total` (point 2), and only for an owner whose UID a Pod of another namespace names. The
   baseline is a release without the correction; a test with such a Pod shows the count before and
   after.
2. **The switches on:** the `can-i` matrix still equal to 0028's.
3. **A full fact queue** at the longest names adds at most 2 MiB (0027's point 10), measured again
   with the switches on.
4. **0028's table at 500 entries,** each at the longest values with a pending `rollout-undo` check,
   takes at most 0.5 MiB (0028's bar 3, not raised); this ADR's share, by difference with
   `reportEnd` off, at most 64 KiB, with the drop times at 100 listed namespaces.
5. **The tick in kind** with 10,000 Pods and 500 pending `rollout-undo` checks, accepted in one
   tick, takes less than 1 second. The `get`s of `replicasets` that point 2's read slot makes in
   that hour are counted against its bound, at most 16 in any 5 seconds, also after the run loop was
   held; the time from each tick to the next stays the tick's own while they run, with the API
   server answering and with its answers held past 5 seconds; and the read slot's size is recorded.
   Again with 32 checks and every answer held 1, 2, 3 and 4 seconds, counting the `get`s per check:
   every check is read at each round, none starves, and a check whose baseline is not taken 900
   seconds after its acceptance ends `not-checkable`. And the peak memory with a ReplicaSet of about
   1.5 MiB of metadata as one check's, read by the read slot, the fact slot and `on_pod` at once,
   against the chart's limit.
6. **500 Incidents closing within one minute**, in kind and on the Raspberry Pis: no watch error,
   relist or restart; the `closed` messages received, plus those counted `failed`, `capped` or
   `dropped`, plus the closings counted `no-incident`, equal the 500 emitted incidents the tracker
   dropped, nothing is counted lost, and no Incident is in two `closed` messages; the `detected`
   messages sent in that hour equal those of the same scenario with `reportEnd` off.
7. **One kind scenario per acting row,** with `W` at 20: `D001` undone by hand gives `observed`,
   with an `ended_at` at most 600 seconds plus one tick plus the rollout's time after the undo, left
   alone `not-observed`, and rolled back just before its Remediation is accepted `not-observed`, not
   `observed` (point 2's forced read); `D003` raised by a rollout, and again by an in-place resize,
   gives `observed`; `D002` with its policy changed gives `observed`; `D014` undone gives
   `observed`; the owner scaled to 0 gives `no-pods`; `D009` closes with `recent-state` and no
   outcome; `D008` gives no `closed`; `D011` on a ReplicaSet whose Event stops gives `basis: event`.
8. **A revoked Pods permission** in a listed namespace closes its open state incidents with
   `source-lost`, and its pending checks end `source-lost`.
9. **Killing the leader** during an open `D001` with a pending check gives no `closed` and no
   `outcome` for that Incident. The failure goes on into the new term, whose leader creates its
   own Incident of the same fingerprint; then the Deployment is fixed by hand, and 330 to 335
   seconds after its last signal the new Incident gets one `closed`.
10. **31 acting Remediations within an hour,** written by a test principal, give 30 `outcome`
    messages and 1 counted `capped`, and change no `detected` count. The scenario: `W` at 20; 31
    `D001` Incidents created before the hour, whose failures go on through it, so that no
    `closed` takes the bound; the 31 Remediations accepted one a minute, at minutes 0 to 30, and
    no Pod changed, so that each check ends `not-observed` 20 minutes after its acceptance, one a
    minute between minutes 20 and 50, inside one 60-minute span, and the queue of 16
    (`QUEUE_LIMIT`, `src/notify.rs:37`) never fills.
11. **The vocabulary test** passes on every enum value and fixed text.
12. **A Pod in another listed namespace M** whose controller `ownerReference` names the
    Deployment of an Incident in N, with its UID, changes nothing in N's outcome check, in its
    `pods_now` or in its `Pods now`.

### Not verified

Each was read in source or documentation only, or not derived; the bar or test that settles it:

- **That an undo gives the restored ReplicaSet a new, higher revision** (the Deployment
  controller's `sync.go` and `deployment_util.go`, read on `master` on 2026-11-24). Bar 7, rows
  `D001` and `D014`.
- **That an in-place resize writes the Pod spec's limit, as the watch sees it.** Bar 7, row `D003`.
- **How long the owner cache's stale revision delays `observed` after an undo,** at most 600 seconds
  and the wait for a read computed: one tick with one check, 320 seconds at 500 checks of 2
  ReplicaSets and 640 at 500 checks of 4, which passes the shortest window by 40 seconds (point 2).
  Bar 7, row `D001`, and a kind test that changes the revision of a running Pod's ReplicaSet (an
  undo to the ReplicaSet of Pods still running) and measures when the check sees it, with the `get`s
  it made.
- **That a write to `pods/status` cannot change a Pod's `metadata.ownerReferences` or its spec**
  (the trust statement's writer of `pods/status`): not read in the API server's source. In kind, a
  PATCH to a Pod's `/status` that changes `metadata.ownerReferences`, and one that changes a
  container's `resources.limits`, are not applied.
- **That the garbage collector deletes a Pod whose controller `ownerReference` names a ReplicaSet
  that does not exist,** with the Deployment's UID, and how soon (the trust statement's writer of
  Pods, point 2's reads). In kind, create such a Pod while a `rollout-undo` check is pending and
  watch it, with the `get`s it costs and the check's result.
- **How long a metadata `get` of a ReplicaSet that does not exist takes on the Raspberry Pis,** a
  `404`, alone and 16 in a row, against the timeout of 5 seconds: bar 5 counts the `get`s in kind,
  not their time on the Pis. Measure it there.
- **That the Deployment controller does not write a ReplicaSet's `deployment.kubernetes.io/revision`
  back** after a writer of ReplicaSets changes it, or how soon it does (the trust statement): not
  read in its source. In kind, patch the annotation of a running Pod's ReplicaSet while a
  `rollout-undo` check is pending, and watch the annotation and the result.
- **That two ReplicaSets of one Deployment never hold the same revision,** on which point 2 gives
  `not-checkable` with no read for more than 4 distinct ReplicaSets: not read in source. If two can,
  such a check is `not-checkable` where it could have been checked, never given a false result. Bar
  7's rows `D001` and `D014` record the revisions they meet.
- **How many `detected` messages enter the queue in one tick** when forged Incidents settle
  together, and so how many end messages they evict. Not derived; a unit test of the tick with
  many forged Events, and bar 6.
- **That the Events of `D011` to `D013` stop while the failure holds** (observations on kind in
  [Detection and redaction rules](../reference/rules.md); the cause a hypothesis). Bar 7, row
  `D011`.
- **That a `403` drop and its relist behave as `on_watch_error` says.** Bar 8.
- **That the garbage collector leaves a Pod in N whose controller `ownerReference` names the
  Deployment with its real UID,** since the owner exists, so that such a Pod lasts until someone
  deletes it; and, a hypothesis that bar 12 does not need, that it treats a reference to an owner
  in another namespace as absent. In kind, create both and watch them for an hour.
- **What a writer of `pods/status` can do against the kubelet:** that a `Failed` phase written by
  hand stays, and how long a clean status written by hand stays before the kubelet writes it
  again. In kind, patch both on a failing Pod and watch the closing and a pending check.
- **The four readings of the Kubernetes source behind the rejected `/status` write**, read at
  `kubernetes/kubernetes` `master` on 2026-11-24 with no commit recorded: that the patch handler
  passes no preconditions (`staging/src/k8s.io/apiserver/pkg/endpoints/handlers/patch.go`, and
  `DefaultUpdatedObjectInfo` in `staging/src/k8s.io/apiserver/pkg/registry/rest/update.go`); that
  a `metadata.uid` in a merge patch to `/status` is then ignored, not refused; that the CRD status
  strategy copies the old object over the new one, UID included
  (`staging/src/k8s.io/apiextensions-apiserver/pkg/registry/customresource/status_strategy.go`);
  and that a rule on `status` does not run when a merge patch sets `status: null`. In kind: create
  an Incident, delete it, create another under its name, and send a merge patch to its `/status`
  with the first one's UID; then send `status: null` with a rule on `status`. The decision does not
  depend on them.
- **The layout of `Instant`, the baseline and the allocator's rounding,** behind the 32 and 80
  bytes, and 0028's 383,000 bytes on which the margin sits. Bar 4, and 0028's bar 3.
- **The tick's CPU with 500 pending checks at 10,000 Pods.** Bar 5.
- **The end bound and the queue rank,** new code: the queue ranks two kinds today. Unit tests, and
  bars 6 and 10.
- **Nothing above is measured.**

## Consequences

- **A receiver learns when kausa stopped seeing an incident**, and, after kurita proposed one of
  three actions, whether the change it makes was seen, without kausa writing or executing anything
  more or reading a new kind of object: it sends the owner lookup's `get` more often. Both are off
  by default; with them off nothing changes but `Pods now`.
- **0027's `Pods now` is corrected with every switch off:** it counts only the Pods of the owner's
  namespace, in the fact statement, the request to the model, the `incident` log line, the
  `Detected` Event and the webhook. It depends on nothing else here, so it can ship ahead of 0029,
  in its own change.
- **kausa still never says "resolved".** Its two new statements are about its own observation and
  about a typed field of the Pods; the banned-word test keeps them so.
- **A missing `closed` is not an open failure:** a restart, a failover, a table entry dropped
  after 6 hours, `D008`, or an incident held back by the gate all leave none. The end of a term
  (`SIGTERM`, the lease lost) counts its closings lost; a table entry dropped after 6 hours and an
  incident held back by the gate count theirs `no-incident`; a crash, a restart on memory included,
  and `D008` count nothing.
- **For `D011` to `D013` a `closed` can come with the failure still there,** because Kubernetes
  stopped updating the Event; its return inside `--dedup-hours` is silent, as today. Stated in the
  message (`basis: event`) and on the reference page.
- **A proposal nobody applied gives `not-observed`** after the window, 60 minutes by default.
- **A writer of Remediations gains one `outcome` message per Incident** and the choice of which
  check runs, never the result; a writer of Events gains one `closed` per forged Incident, and the
  `closed` of a genuine event incident whose Event it deletes; neither costs a `detected` or an
  `explained` message. An end message never evicts a `remediation`; the `detected` messages of
  forged Incidents evict queued end messages first and, once none is queued, a `remediation`, by
  0028's rule.
- **A writer of ReplicaSets in a namespace can forge an `observed` there,** by writing the revision
  annotation of the Pods' ReplicaSets, and can as well roll the Deployment back for real. The reads
  the checks need run in a slot of their own, at most 16 a tick, so no writer of Pods, ReplicaSets
  or Remediations makes them hold the run loop.
- **The end bound is not fair:** it is one for the process, so a writer of Events in one namespace,
  with 30 forged Incidents an hour, one every 120 seconds, leaves no `closed` and no `outcome`
  message about any namespace for that hour; each is counted `capped` and logged, the log line of
  each end is written, and nothing is lost uncounted while the process lives.
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
