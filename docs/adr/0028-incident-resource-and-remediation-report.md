# 0028. Publish each incident as an immutable Incident that only kausa writes, and report by webhook what kurita proposed for it, both off by default

- Status: proposed. Acceptance needs the bars of "Conditions of acceptance" measured, and
  kurita's ADR 0008, "Contract with kausa", the other half of this contract, accepted. Until then
  kausa writes no Incident and reads no Remediation, and the ADRs this one amends hold as written.
- Date: 2026-11-24
- Builds on: [0014. Keep each incident's full explanations in a namespaced Explanation resource,
  off by default](0014-explanation-history.md) (a resource of kausa's own, its sweep, its cap and
  its reader role) and [0027. Report each settled incident as a fact statement in its own
  `Detected` Event and webhook message, with or without a model, and mark the model's text as a
  model's](0027-fact-statement-for-the-detections.md) (the fact slot, the fact queue, the facts'
  vocabulary and the webhook's `v2`)
- Amends, when accepted, each with an `Update 2026-11-24` paragraph at its end: [0001. Build a
  small, read-only explainer that writes one redacted explanation per
  incident](0001-scope-and-positioning.md) (what kausa writes); [0009. Run with read-only
  permissions that never include Secrets](0009-least-privilege-rbac.md) (the permissions);
  [0015. Post each new explanation to one optional webhook, as JSON, Slack or
  Telegram](0015-notifications.md) (a new kind of message); [0027. Report each settled incident as
  a fact statement in its own `Detected` Event and webhook message, with or without a model, and
  mark the model's text as a model's](0027-fact-statement-for-the-detections.md) (its points 3, 5
  and 10); and invariants 2, 5, 11 and 12 of [ARCHITECTURE.md](../../ARCHITECTURE.md), changed
  with the code
- Keeps: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md): nothing new goes to the model

kurita is a separate project, a controller that proposes a remediation for an incident kausa
reports. Its ADR 0008, the contract with kausa, is written at the same time as this one and decides
kurita's half; where this ADR restates that half, it is so that kausa's half can be read alone, and
kurita's ADR 0008 holds. Every figure of memory,
size, writes and latency below is **computed, not measured**, unless it says measured.

## Context

Status as of 2026-11-24: `main` at `4b85369`, after the merge that implements and accepts 0027, and
v0.6.3 released; every path and line cited below is `main`'s. Nothing below is implemented. kurita
is not published as of 2026-11-24, so its ADRs are cited here by number and title, without links,
until it is.

**What another controller would need, and kausa has no way to give.** kausa reports an incident
in its log, in a `Detected` Event and, with a webhook, in a `detected` message (0027). A
controller that proposes a remediation needs the same incident as typed data, from a source that
only kausa can write. None of today's outputs is one:

- **An Event is forgeable.** Whoever can create Events in a namespace, the built-in `edit` role
  included, can write one that looks like kausa's (0027's acceptance `Update`, its item "A
  `Detected` Event's name can be taken first"). Events are kept one
  hour by default (`kube-apiserver --event-ttl`), and their note is text.
- **The log and the webhook leave the cluster** and are not a resource another controller reads.
- **An Explanation** (0014) holds the model's text, exists only with a model, and is patched.

kurita's ADR 0008 also needs data kausa does not keep today (kausa's source as of 2026-11-24):

- A Deployment's generation and revision are held only while the rollout rule (`D014`) matches it.
  For `D001` to `D005` nothing about the Deployment is held.
- An incident lists its Pods by name, up to 100. Its owner carries a UID, which may be empty for an
  Event incident, since whoever writes the Event chooses it (`src/incident.rs:190`).
- An Event incident about something other than a Pod keeps the kind of that object, not its name.
- The owner of a Pod is found through a metadata `get` of its ReplicaSet or Job, cached by hop,
  namespace and name for 600 seconds (`src/run.rs:82`, `src/run.rs:1902-1953`).
- The fact queue holds 1024 entries of 1,336 bytes, 1,368,064 bytes (1.30 MiB) when full, pinned
  by a unit test (`src/report.rs:309-313`), against the bar of 0027's point 10 that a full queue
  adds at most 2 MiB.

**How the design was reached.** Two complete designs were written against the same brief and fixed
limits, and a review chose between them point by point, checking each one's claims against
kausa's source, kurita's ADRs and the Kubernetes documentation and source. The design below is the
result; "Alternatives considered" lists what was rejected and why. Two points the review left to
the maintainer are decided: the webhook's `v2` is amended before its first release so that a new
`type` fits in it (0027's acceptance `Update`, its item "Within `v2` a value of `type` may be
added"), and kurita compares the ReplicaSet's revision and
UID for `D001` to `D005`, so kausa keeps no table of every Deployment.

The fixed limits of that brief, which this ADR keeps: kausa stays read-only towards workloads and
reads no Secret and no ConfigMap; kurita v0.1 only proposes; only typed fields of a remediation
leave by kausa's webhook, and nothing new goes to the model; what a report depends on travels in a
resource that, by RBAC, only the two controllers write; the two projects couple only through
cluster resources, each works without the other, and neither chart depends on the other; every
string in either schema is an enum, or a pattern with a maximum length.

## Alternatives considered

- **The `Detected` Event as the carrier.** Forgeable by every Event creator, `edit` included; kept
  one hour by default; free text in its note; and kurita would need `list` and `watch` on Events.
- **One mutable Incident per fingerprint, with kurita's record in its `status`.** It needs `patch`
  and `watch` on incidents for kausa, more verbs than this design; the values at detection move
  under the record that cites them; and the object kausa would report is read back from a record
  that its writers can change. Fewer creates under flapping, but the create gate of point 1 removes
  most of them.
- **kausa writing the result of its later outcome check into `Remediation.status`.** Two writers on
  one object, and kausa gains a write on another project's resource. The outcome check attaches to
  `Incident.status` instead (point 9).
- **An `ownerReference` from each Remediation to its Incident.** kausa's retention would delete
  kurita's records through the garbage collector, and kurita counts its budget from them.
- **Cluster-scoped resources, or one central namespace.** They need a ClusterRole, which the
  namespaced install mode ([0013](0013-namespaced-install-mode.md)) avoids; tenants lose `view` on
  their own incidents; an `ownerReference` cannot cross namespaces.
- **A third chart that ships both resources.** An install dependency between the projects. Each
  chart ships the resource its controller writes.
- **A table of every Deployment's generation and revision, kept by kausa.** It closes kurita's
  change check for `D001` to `D005` exactly, at a cost in memory proportional to the cluster (about
  120 bytes a Deployment, an estimate of the design that proposed it, not derived or measured
  here). Neither it nor the ReplicaSet revision of
  point 3 gives a forger less or a controller fewer permissions, so the cost decided; kurita's ADR
  0008 (its point 4) states what the revision misses. Kept as the fallback of kurita's execution
  ADR.
- **A bar that lets a full fact queue add up to 3 MiB.** It would loosen 0027's point 10 to fit a
  design whose queue did not meet 2 MiB. The extra data was trimmed instead (point 3), and the bar
  stays.
- **Carrying the incident's Pods with their UIDs.** Up to 100 Pods at about 293 bytes each (a name
  of at most 253 bytes, a 24-byte string header and a 16-byte binary UID): 29,300 bytes an entry,
  and 29,300 × 1024 = 30,003,200 bytes, about 28.6 MiB, for a full fact queue, far past the 2 MiB
  bar. kurita lists the Pods live.
- **A `probe` field taken from the note of an `Unhealthy` Event.** An enum derived from text that
  whoever writes the Event chooses, and nothing in kurita consumes it.
- **The memory limit as a quantity string**, in kurita's resource. The pattern proposed for it
  rejected `512Ki` and accepted `1ki`. Integer bytes instead.
- **Reporting any valid Remediation**, with the object read back from it. It sends data read back
  from a writable resource, against invariant 5, and after a restart kausa could not tell which
  records it had already sent without a write. kausa reports only for Incidents it created in its
  own process (point 6); the cost is a lost notice after a restart, never a false one.
- **`get`, `patch` or `watch` on incidents for kausa.** Not needed: kausa creates, sweeps, and
  finds its Incidents in its own memory (point 6).
- **A new webhook version `v3` for the remediation message.** `v2` is not released, so its
  compatibility text is amended instead (0027's acceptance `Update`, its item "Within `v2` a value
  of `type` may be added").
- **ConfigMaps, or annotations on workloads.** kausa reads no ConfigMap and writes no workload.
- **Keeping the revision with each Pod's resolved owner**, read when the owner is resolved, with
  incidents on. 8 bytes a Pod, 80,000 bytes at 10,000 Pods, and no request at the create; but as old
  as the Pod's last resolution, which for a running Pod is its creation, hours or days. A rollback
  that reuses the Pod's ReplicaSet gives it a new revision, and every Incident about its older Pods
  would then carry the old one, so kurita's equality would stop each undo proposal for as long as
  those Pods live. The read in the fact slot (point 3) is at most 600 seconds old for at most one
  `get` per create attempted.
- **A third value of `missing` for a revision with no fresh entry, and nothing else.** At the
  emitting tick the entry of a Pod resolved more than 10 minutes earlier is usually gone (point 3),
  so most Incidents about a long-running workload would carry no revision, and kurita would almost
  never propose an undo for one, which is what the contract is for.

## Decision

Proposed on 2026-11-24. Both switches are off by default, and with the defaults kausa writes, reads
and sends what it does today, but for two changes, both from the owner lookup's rule on names of
point 3, which holds with incidents off too (**Update 2026-11-24:** it shipped ahead, in its own
change): kausa sends no `get` for a name that is not a DNS-1123 subdomain of at most 253 characters,
or for a namespace that is not a DNS-1123 label, so it reads less; and an incident whose owner has
such a name, which today could resolve to another object, names the `ownerReference` or the Event's
object as given instead, in its Events, its `incident` log line and its `detected` message.

### 1. The Incident

kausa publishes each incident as one object of a new resource, `incidents.kausa.fedeam.com`,
version `v1alpha1`, kind `Incident`:

- **Namespaced**, in the namespace of the workload it is about, named
  `kausa.<fingerprint>.<firstSeenUnix>` (the 16 hexadecimal characters of the fingerprint and the
  incident's first-seen time in Unix seconds), so one name per occurrence. The seconds have 1 to 10
  digits, as kurita parses the name, `^kausa\.[0-9a-f]{16}\.[0-9]{1,10}$`: a `firstSeen` before
  1970-01-01T00:00:00Z, or at or after 2286-11-20T17:46:40Z (10^10 seconds), gets no Incident, a
  refusal with the reason `schema` (point 2).
- **Immutable.** It is written by one `create` and never patched. `spec` is required at the root
  of the schema, and a CEL rule `self == oldSelf` on it refuses any change of the `spec`, for every
  writer. The `spec` must be required: a transition rule is not evaluated when there is no old
  value, so without it an object created with no `spec` could be given one later. The `status`
  subresource is declared and empty: it is the place of the outcome check (point 9).
- **Created in the fact slot of 0027's point 3**, after the incident's `Detected` Event (at once
  with `--no-events`) and, for a sample Pod run by a ReplicaSet, after the read of its revision
  (point 3), within the 10 seconds a write has there, and never before the incident's
  `incident` log line. A failed create is logged and counted (this ADR proposes
  `kausa_incident_write_errors_total`; the reference page fixes the name with the code) and not
  retried. A create answered `409 AlreadyExists` is such a failure, and its fingerprint is not
  marked: the table keeps an entry only with the UID the server gave a create it accepted.
- **At most one create per fingerprint per `--dedup-hours`**, the window of 0027's point 5 (6 by
  default; with `0`, every occurrence). The gate reads kausa's own table of the Incidents it created
  in this process (below), never a list of the resource, and a fingerprint is marked only after a
  create the server accepted. The table holds one entry per Incident; the gate looks it up by
  namespace and fingerprint, as kurita keys its budget (its ADR 0008, point 8), and an entry with
  both, created inside the window, refuses the create: the fingerprint already hashes the namespace
  (`src/fingerprint.rs`), but it is not cryptographic. This table, not 0027's table of notified
  fingerprints (1024 entries), is the gate, and it holds at most 500 entries under the rule below:
  an entry younger than 6 hours is never dropped, so with `--dedup-hours` at 6 or less the gate
  holds for the whole window; with more than 6, an entry older than 6 hours can be dropped to make
  room, and then a second Incident for that fingerprint inside the window is possible, as after a
  restart (point 10). kurita's budget per fingerprint absorbs it (`needs-human{cooldown}`) when the
  first Incident's record counts in that window, and otherwise kurita decides the second like any
  other Incident; a record counts when its action is `inspect` or an acting id (kurita's ADR 0008,
  point 8).
- **The table of created Incidents**, kept with incidents on, report or not: one entry per
  Incident kausa created in this process, with the time it created it on kausa's own monotonic
  clock, at most 500 entries. That time is taken when the server's answer accepting the create
  arrives, never before the request: kurita measures its 6 hours from `creationTimestamp`, which
  the server sets while it handles the create and truncates to the second, so kausa's 6 hours
  start no earlier than kurita's and end later by the time the answer took, at most the 10 seconds
  of the write, plus the truncation, under 1 second. The difference errs towards keeping an entry
  a few seconds longer than kurita can use it, which costs at most a slot for those seconds and
  loses no notice; taken before the request, kausa's 6 hours could end first, and a Remediation
  kurita created in those last seconds could find the entry dropped and go unreported
  (`incident-unknown`). A clock of kurita's behind the API server's moves its own window later, out
  of kausa's reach. **It never drops an entry younger than 6 hours**, the window in
  which kurita decides an Incident (its ADR 0008, point 3); an entry older than that is dropped,
  oldest first, when room is needed. It is never seeded from a list of Incidents (point 10).
- **While the table holds 500 entries younger than 6 hours, kausa creates no Incident.** The
  occurrence is counted (this ADR proposes `kausa_incidents_refused_total`, with a label `reason` of
  three values, `table-full` for this stop, `schema` for point 2's rule and `starting` for a create
  that arrives before the sweep at start ended (point 4); the reference page fixes the names) and
  logged as `incident refused` at most once an hour for each reason; the incident's Events, its
  `incident` log line and its `detected` message are not affected, and its fingerprint is not
  marked, so a later occurrence creates one once there is room. So a process creates at most 500
  Incidents in any 6 hours: every Incident it created in the last 6 hours is still in the table, and
  the table holds at most 500. The one exception is a create the server stored but kausa got no
  valid answer for: no answer within the 10 seconds, an answer over 64 KiB, or one with no UID.
  kausa then inserts no entry and marks no fingerprint, and that create does not count toward the
  500, so a later occurrence of the incident creates again. Such creates are bounded only by the
  rate of the fact slot, which writes one fact at a time (0027's point 3), and the Incidents they
  leave by the hourly sweep's cap of 500 (point 4). A Remediation about such an Incident finds no
  entry and is not reported (`incident-unknown`). The label is the manager's decision, from the
  security reader's ninth review: with one counter and one line an hour, one refusal of point 2's
  rule an hour, which a writer of Pods or Events in N can cause, would take the line and hide this
  stop, which holds Incidents back in every namespace. The label's values are a closed set and name
  nothing of the cluster, as 0027's counters do, and the three series show 0 from the start with
  incidents on. The hour is kept per reason and across namespaces, so the line names the first
  refusal of the hour for its reason and the counter counts every one.
- **No `ownerReference`** to the workload or to anything else, and nothing points to it.
- **Not with `--dry-run`**, as `--history`: kausa refuses to start with incidents on and
  `--dry-run`, and the chart turns incidents off with `dryRun`.
- **The chart ships the resource as a template**, as it ships Explanations: turning incidents off
  or uninstalling deletes the resource and every Incident in the cluster. As 0014 decided for
  Explanations, `incidents.installCRD` (true by default) says whether the release installs the
  CustomResourceDefinition and the reader role of point 5; `false` when another release or a
  cluster administrator installs them, as a namespaced install ([0013](0013-namespaced-install-mode.md))
  needs.

### 2. Its schema is the second allowlist

The Incident's `spec`, structural, with no field kept unknown (a sketch; the reference page comes
with the code):

```yaml
spec:                       # required; CEL: self == oldSelf
  detection: D001..D014
  fingerprint: ^[0-9a-f]{16}$
  firstSeen: Time
  settledAt: Time
  object:                   # the incident's top-level owner
    apiVersion: at most 253, a group/version pattern: a group and a version, or a version
                alone; the version starts with a letter
    kind: ^[A-Z][A-Za-z0-9]*$, at most 63
    name: Name
    uid: UID                # optional: an Event incident's owner may have none
  facts:                    # 0027's point 5 vocabulary
    reason: the compiled list of src/fact.rs   # optional
    exitCode: int32                            # optional
    container: DNS label, at most 63           # optional
    podsSeen: 0..100        # 0 exactly for D008 and D011 to D014; capped at 100
    podsSeenIsMinimum: bool
    podsNow: 0..1000000
  sample:                   # optional; CEL: present exactly when podsSeen > 0
    pod: {name: Name, uid: UID}
    controller:             # optional: absent for a Pod with no controller ownerReference
      kind: ReplicaSet | Job | StatefulSet | DaemonSet | Other
      uid: UID
      revision: int64 1..2^53   # optional: absent exactly when missing names
                                # revision-lookup-failed (the fact slot's read of point 3
                                # failed, answered with another UID than the Pod's
                                # ownerReference, or found the annotation absent or not a
                                # whole number in 1..2^53); CEL: only with kind ReplicaSet
  deployment: {generation: int64 >= 1, revision: int64 1..2^53}   # CEL: exactly for D014
  regarding:                # an Event incident about something other than a Pod; CEL: never
                            # with sample
    kind: Job | ReplicaSet | StatefulSet | DaemonSet | CronJob | PersistentVolumeClaim
          | HorizontalPodAutoscaler
    name: Name
    uid: UID                # optional
  missing: at most 2 of owner-lookup-failed | revision-lookup-failed, a set
                            # CEL: owner-lookup-failed only with sample or for D008 and D011,
                            # and for those two only with a regarding whose kind, name and uid
                            # (both absent or equal) are object's, and an object that is a Job
                            # of batch/v1 or a ReplicaSet of apps/v1
```

`Time` is the pattern `^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$` with at most 20
characters; `UID` a UID pattern of 36 characters; `Name` a DNS-1123 subdomain of at most 253. Every
string is an enum, or a pattern with a maximum length; a test walks the schema to check it, and
another checks that the `reason` enum equals the list compiled in `src/fact.rs`. At the longest
names the `spec` is about 1.5 KB (an estimate; bar 6 measures the stored object).

`facts` holds 0027's point 5 vocabulary but its `revision` (`src/fact.rs:101`), which only `D014`
has: that revision travels in `deployment.revision`, with the generation, and not twice.

The schema is kausa's second allowlist, beside invariant 12's fact statement: an Incident holds
only the fact statement's facts and the typed values listed here, never a message, a note, a log
line, an image, a label, a node or any text a model wrote. Changing what it may hold needs an ADR,
and invariant 12 says so (point 8). Within `v1alpha1` changes are additive only.

**kausa never sends a create the schema would refuse.** This rule is the ADR author's, added after
an eighth review. Part of an Incident comes from objects a writer in N chooses: the
`ownerReferences` of a Pod and of its ReplicaSet, an Event's `regarding` and times, the revision
annotation of a ReplicaSet or a Deployment. The API server asks little of them: in kind it accepted
a line break in an ownerReference's kind and name and in an Event's `regarding` ([2026-11-24 — The
fact statement](../journal/2026-11-24-the-fact-statement.md)), and that it asks an ownerReference's
UID only to be non-empty is under "Not verified". So kausa checks each value against its field, by
the patterns, ranges, enums and CEL rules above and the rule for the Incident's name, when the tick
builds the fact queue entry, and the revision again after point 3's read; a value that does not fit
is never kept and never sent:

- **`sample.controller.revision`**, the one value with a code of `missing`, is left out with
  `revision-lookup-failed` when the annotation is absent or not a whole number in 1..2^53 (point 3).
- **Every other value read from the cluster**, required or optional: `object`'s `apiVersion`,
  `kind`, `name` and `uid`; `sample.controller.uid`; `regarding`'s `kind`, `name` and `uid`;
  `deployment.generation` of `D014`, so a `D014` with no generation is refused, and its
  `deployment.revision`, which the fact statement parses as any `u64` (`src/fact.rs:135-138`); and
  `firstSeen`, which also sets the name. An owner's name the lookup
  refused (point 3) is one of these. One that does not fit means no create: the incident's Events,
  its `incident` log line and its `detected` message are not affected; the occurrence is counted and
  logged as a refusal of point 1 with the reason `schema`
  (`kausa_incidents_refused_total{reason="schema"}`, `incident refused` at most once an hour for
  that reason, naming the field and the namespace, never the value); no `get` is made for it; and
  its fingerprint is not marked, so a later occurrence is checked again, at no request. None is left
  out instead: `missing` has no code for it, and an optional `uid` left out would say the object has
  none.
- **The values kausa makes** fit by construction: `detection`, `fingerprint`, `settledAt`,
  `missing`, `sample.controller.kind` (any other kind is `Other`), and `facts`, since the fact
  statement admits only a listed reason, an `i32` exit code and a container that is a DNS label
  (`src/fact.rs:115-139`), and `podsSeen` is capped at 100 (next bullet); a `podsNow` above
  1,000,000 is a no create, as above. `sample.pod` comes from a Pod's own metadata, whose name the
  API server validates and whose UID it sets.
- **`podsSeen`, 0 to 100, so the rule holds for it too.** 0 means that no Pod was seen: the incident
  is about an object that is not a Pod, which is `D008` and `D011` to `D014` and only those
  (`src/incident.rs:185-192` and `:236-245` give their signals no Pod; a test at `:917`). Each
  detection is about a Pod or never is, since the kind of the object an Event is about is part of
  each match (`src/detect.rs:235-294`), so every signal of `D001` to `D007`, `D009` and `D010` names
  its Pod. The fact statement leaves a count of 0 out of its text (`src/fact.rs:166`); the Incident,
  a typed record, keeps it, and a reader keeps one integer with no optional. The count can pass 100:
  the set of Pods is capped at 100 while the incident is pending, and the Pods failing when it
  settles are added past the cap (`src/incident.rs:485-487`; a test at `:1227-1234` has 101, and the
  fact statement then reads "at least 101", `src/fact.rs:425-429`). The Incident holds the count up
  to 100 and 100 above it, and `podsSeenIsMinimum` is the fact statement's own flag, true when the
  count is 100 or more (`src/fact.rs:133`), so "at least 100" stays true. At 0 `podsSeenIsMinimum`
  is false, by the same comparison. `sample` is absent exactly when `podsSeen` is 0: point 3 sets it
  for every incident of `D001` to `D007`, `D009` and `D010`, whose set holds at least the sample's
  Pod (the sample is taken from the group whose Pods `src/incident.rs:487` adds), and no other
  incident has one; a CEL rule on `spec` checks it, within bar 7's cost. The range 0..100 is the
  manager's decision, under the plan's authority to decide ADRs within its invariants, after the
  eighth review; capping the count at 100 is the ADR author's. kurita's ADR 0008's "1 to 100"
  becomes "0 to 100" in the change there.

### 3. The data kausa keeps for it

Each entry of the fact queue gains one optional pointer, filled only with incidents on:

- **The sample Pod's UID**, from the record of the incident's sample Pod, taken in the tick that
  emits the incident (`src/run.rs:1206-1220`). An incident born from a Pod's state or from an Event
  about a Pod (`D001` to `D007`, `D009` and `D010`) is emitted only at a tick where each of its
  signals comes from a Pod record kausa holds (`src/incident.rs:99`, `:194-197`), and every such
  record has a UID (`src/record.rs:548`), so for those `sample.pod` is always set. `D008`, `D011` to
  `D014` have no sample Pod, and no `sample`. Update 2026-11-25: the record is found by the UID the
  sample's signal carries (`Signal.pod_uid`, the record's own for a Pod's state, and for an Event
  about a Pod the UID of the record the tick takes the Pod's owner from), never by name: while a
  relist is open, kausa holds the old record of a Pod deleted in a watch gap beside its replacement
  of the same name (`on_pod` drops a vanished UID only at `InitDone`), and by name the data could
  name the one that is not failing.
- **The sample Pod's controller, when the Pod has a controller `ownerReference`:** its kind and its
  UID, from the Pod's own `ownerReference`, and, for a ReplicaSet, its name and the revision in its
  `deployment.kubernetes.io/revision` annotation, read in the fact slot as the next paragraph says.
  The name is kept only until that read and is not written to the Incident: kurita finds the
  ReplicaSet by UID with its own `list`. An entry holds a `sample` or a `regarding`, never both (an
  incident born from a Pod has no `regarding`, one about another object no sample Pod), so this name
  and `regarding`'s share one field.
- **`regarding`**: the kind, the name and, when the Event has one, the UID of the object an Event
  incident is about, when that object is not a Pod.
- **`missing`**: which of the two reads failed. `owner-lookup-failed`: the owner lookup failed when
  kausa resolved the incident's owner, so the incident names the ReplicaSet or the Job in place of
  its owner. For an incident about a Pod, that is the sample Pod's lookup (the third value of a
  Pod's entry, `src/run.rs:1399-1401`); for an Event incident about a Job or a ReplicaSet (`D008`,
  `D011`), the lookup of the Event's object (`src/run.rs:1540-1548`), whose failure an Event's entry
  does not keep today (`:1410`, only logged): the entry gains the same flag, kept with incidents off
  too, and then `object` and `regarding` name the same Job or ReplicaSet: same kind, name and UID,
  and an `object` that is a Job of `batch/v1` or a ReplicaSet of `apps/v1`, the two kurita's parse
  admits (a CEL rule on `spec` and the check say it in these words). Both are taken in the
  tick. `revision-lookup-failed`: the read of the revision in the fact slot found none, because its
  `get` failed or gave no answer within 5 seconds, because it answered for a ReplicaSet with another
  UID than the one in the Pod's `ownerReference`, or because the annotation is absent or not a whole
  number in 1..2^53 (point 2's rule). `sample.controller.revision` of a ReplicaSet is absent exactly
  when `missing` names `revision-lookup-failed`.
- **For `D014`**, the Deployment's `metadata.generation` and revision at detection, from the record
  kausa already holds while the rule matches.

UIDs are kept as 16 binary bytes. Each owner-cache entry gains one optional pointer, 8 bytes, empty
with incidents off; with incidents on, the entry of a ReplicaSet whose `get` answered points to the
revision and the UID that `get` returned, 8 and 16 bytes. No table of Deployments is added, and no
Pod beyond the sample.

**The owner lookup checks the name it is given.** This rule is the ADR author's, added after an
eighth review, and it is the lookup's own, so it holds for every caller and with incidents off too:
`on_pod`'s resolution of a Pod's owner and of an Event's object (`src/run.rs:1463`, `:1542`) and the
fact slot's read of the revision below. At `4b85369` the name comes from an `ownerReference` or an
Event's `regarding`, copied unbounded (an `ownerReference` at `src/record.rs:228-235`, a `regarding`
at `src/run.rs:1534-1539`); `controller_of` (`src/run.rs:1909-1953`) keys its cache by it and passes
it to `get_metadata`, which in kube-core 4.2.0 refuses only an empty name (`validate_name`,
`src/request.rs:338-343`) and puts it in the path unencoded (`src/request.rs:274`); and the API
server asks an ownerReference's name, by a reviewer's reading not re-read here, only to be
non-empty. So a writer of Pods in N chooses a string of any length with `/`, `..` or `?` that kausa
keeps as a key and sends in a URL. Under this rule a name that is not a DNS-1123 subdomain of at
most 253 characters, or a namespace that is not a DNS-1123 label (an Event's `regarding.namespace`
is the other half of the same path), is never looked up and never kept: the lookup answers as a
failed one without a request and without a cache entry. For the owner, that gives
`owner-lookup-failed`, and the incident's `object` is the `ownerReference` as the Pod gives it, as
after any failed lookup (`top_owner`, `src/owner.rs:67`), with its Events and its log line as today;
its name does not fit `Name`, so point 2's rule creates no Incident for it. For the revision it
gives `revision-lookup-failed`, and the name is not kept in the fact queue entry. A name that passes
has no `/`, no `?` and no `..` segment. That no Secret or ConfigMap can be read through such a path
rests on the code, which refuses the name and the namespace (**Update 2026-11-24:** shipped ahead),
and on RBAC, which grants kausa no verb on them ([Permissions](../reference/permissions.md))
besides. What the `owner lookup failed` line prints of the name and the namespace is cut at 253
characters.

**Where the revision comes from.** This paragraph was written after the design review, which left
the read implicit: it is this ADR's own design, not one of the two compared. kausa reads a
ReplicaSet's revision only through the owner lookup (`controller_of`, `src/run.rs:1909-1953`), whose
cache entry holds it, and only from async code; the tick, which is synchronous (`src/run.rs:1636`),
makes no request. The tick cannot use the entry either: a Pod's owner is resolved once and reused
while its controller reference stays the same and the lookup worked (`src/run.rs:1448-1452`), and an
entry expires after 600 seconds (`OWNER_CACHE_TTL`, `:82`) and is dropped only when a later lookup
misses the cache and sends a `get` (`retain`, `:1950`), so at the tick that emits an incident about
a Pod older than 10 minutes the sample's entry is usually expired or gone. The fact slot reads it
instead, and only for a create kausa will send: after the incident's `Detected` Event, after the
gate of point 1 admits the create and after the table of created Incidents is found to have room,
and before the create. It calls the lookup for the sample's ReplicaSet: a fresh entry answers with
no request; an absent or expired one costs the existing metadata `get` of `replicasets`, with its
timeout of 5 seconds (`OWNER_LOOKUP_TIMEOUT`, `:89`); a failed one is answered from the cache for 30
seconds (`OWNER_ERROR_TTL`, `:83`). An answer whose `metadata.uid` or revision annotation cannot be
read is kept as a failed read for the answer's 600 seconds, not the error's 30: the 30 seconds are
for a request that failed. A ReplicaSet's revision is read in one strict spelling (digits, no sign,
no leading zero, 1 to 2^53; `parse_revision`, `src/record.rs:810`); a Deployment's, for `D014`, is
the fact statement's parse (`src/record.rs:871-875`, any spelling Rust's `u64` parse takes, such as
`+7` or `007`), then bounded 1 to 2^53 by the check. The slot compares the UID the entry's `get`
returned with the UID in the Pod's `ownerReference`, and gives `revision-lookup-failed` when they
differ. The slot can
reach the lookup as the code stands: the term's lookup (`src/run.rs:1028`) is created before the
slot's future, which is assigned at `:1071` (`reporter.report(fact, &lookup)`; `reporting` is
declared at `:1060`), and outlives it; it is passed to `Reporter::report` as an argument, since the
reporter outlives terms; and its cache is a `RefCell` (`:2084`) never borrowed across an `await`,
so the slot and `on_pod` share it. So:

- **Fresh at the read, within 600 seconds, and of the same ReplicaSet.** The owner cache keys an
  entry by hop, namespace and name, not by UID; the UID kept in the entry makes a ReplicaSet deleted
  and recreated under the same name within 600 seconds, or a Pod whose reference pairs one
  ReplicaSet's name with another's UID, give `revision-lookup-failed`. What remains is the staleness
  of the same ReplicaSet: its revision changes only when a rollback reuses it, which gives it a
  revision higher than any other of its Deployment's, so a stale value is lower than its live one
  and than the Deployment's live revision. kurita compares for equality (its ADR 0008, point 4), so
  a stale revision stops an undo proposal and never allows one, as long as a ReplicaSet's revision
  only grows and a Deployment's revision is never lower than any of its ReplicaSets' ("Not
  verified"); a failed read stops it too. A proposal is missed this way only when the ReplicaSet's
  revision changed in the 600 seconds before the read.
- **The revision at the read, not at detection.** The entry can have been filled after the tick that
  emitted the incident, so the Incident carries the revision as of the read, and kurita's point 4
  detects changes from that read on, not from detection. A rollback onto the sample's ReplicaSet
  between the tick and the read (the time the fact waited in the queue and its `Detected` Event
  took) gives it the Deployment's highest revision; the Incident carries it, the newest ReplicaSet
  is the sample's, and kurita's comparison passes where a value from detection would have stopped
  it. That needs a ReplicaSet that was not the newest at detection, and a writer of Deployments in
  N, which can make any change to that Deployment itself; what it can get is an undo proposal of
  that rollback, made only if kurita's live reads confirm the failure, and executed by nobody in
  kurita v0.1. Whether kurita's rows reach that proposal is under "Not verified".
- **At most one `get` per create attempted**, none for a create the gate or the table refuses. A
  create the server accepts happens at most 500 times in any 6 hours per process (point 1). A failed
  create is not retried and its fingerprint is not marked (point 1), so the fingerprint's next
  occurrence is a new attempt and reads again; for those, the cache is the bound: at most one `get`
  per ReplicaSet name every 600 seconds while its `get` answers, 36 in 6 hours, or every 30 seconds
  while it fails, 720 in 6 hours. Both figures are per name: they multiply by the ReplicaSets whose
  Pods fail, and a writer of Pods in N chooses those names, within the Pods it may create. Each
  holds the fact slot at most 5 seconds, before the create and its own 10 seconds. No new verb and
  no new watch.
- **With incidents off,** no read is made, the revision is not parsed and nothing of the read is
  kept: the fact queue entry's pointer and the owner-cache entry's stay empty. The Event entry's
  flag of `owner-lookup-failed` (above) is kept with incidents off too.

### 4. Retention

| Limit | Set by | Default |
|---|---|---|
| Age, by `metadata.creationTimestamp` | `incidents.days`, 1 to 30 | 1 |
| Incidents per kausa | constant, the same as 0014's `MAX_OBJECTS` | 500 |

- **0014's sweep, applied to Incidents**: at start and hourly, for at most 30 seconds, in pages of 2
  (0014's `Update 2026-10-28`), dropping the `metadata` it does not read before it reduces each
  object. It deletes, with UID and `resourceVersion` preconditions, the Incidents older than
  `incidents.days` and, through a bounded heap, the oldest beyond 500. It runs in a slot of its own,
  at the start of each term and then hourly (`src/run.rs:1062-1085`, the condition at `:1079`),
  under invariant 6: a fact never waits for it longer than for a write.
- **"Oldest" is by `metadata.creationTimestamp`**, which the API server sets and no writer can,
  never by `firstSeen`, which any writer of Incidents sets.
- **One day covers kurita's window**: kurita decides an Incident only while it is younger than 6
  hours by `creationTimestamp` (its ADR 0008, point 3). The outcome check may raise the default.
- **The cap of 500 is the sweep's target, and kausa's own creates are bounded apart.** The sweep at
  start ends before the first create of the process: a create that arrives before it ended,
  completed or stopped (at its 30 seconds, or at a list or a delete with no answer within the 10
  seconds of a write, an ending that lets creates through as well), does not wait for it, and is
  refused with the reason `starting` (point 1), counted and logged as the other refusals, its
  fingerprint not marked, so a later occurrence creates one. A sweep that failed, a list or a delete
  refused by the server (the CustomResourceDefinition missing among them), has ended too and lets
  creates through: the count is then not trimmed, and the process's own 500 in any 6 hours (point 1)
  still bounds its creates. A sweep cut by the loss of the term has not ended, and the next term's
  sweep counts. Between two sweeps one process creates at most 500 Incidents (point 1: at most 500
  in any 6 hours, and the sweeps are an hour apart), so after a sweep that completes the count kausa
  causes stays at most 500 + 500 = 1,000 until the next one. A restart or a failover empties the
  table, so each process start allows 500 more: in a crash loop, each start whose sweep does not
  complete before the process ends can leave up to 500 more Incidents, and the next sweep that
  completes trims the count to 500 again (bar 8 measures that it does within its 30 seconds).
  Incidents written by anyone else are not bounded by this ADR, as for Explanations (0014).
- **The cap is shared across namespaces**, as 0014 says of its own: it protects the datastore, not
  fairness. The sweep deletes the oldest Incidents beyond 500 whatever their namespace; only
  Incidents written by someone other than kausa, or left by earlier processes, can push it to
  delete ones younger than 6 hours, since one process holds at most 500 of those. kausa's own
  table is per process and is the bound on creates: whoever makes kausa create 500 Incidents in 6
  hours, a tenant whose workloads fail that often or a writer of Events (trust statement), stops
  new Incidents in every namespace until the oldest entry passes 6 hours, and for as long as it
  goes on: holding the stop takes one create every 6 × 3,600 / 500 = 43.2 seconds, so one forged
  Event with a new fingerprint every 43.2 seconds holds it indefinitely. With `--dedup-hours` at 6
  or more, 500 creates take 500 different fingerprints; with less, fewer, down to one fingerprint
  that recurs at `0`, since the gate of point 1 then lets each occurrence create. That
  moves kurita towards not proposing; kausa still writes its Events and posts its `detected`
  messages, and a Remediation of an Incident already in the table is still reported. A
  ResourceQuota on `count/incidents.kausa.fedeam.com` in a namespace bounds what that namespace can
  hold, against writers of Incidents; it does not bound kausa's table.

### 5. Permissions

Through 0013's `kausa.rules` (the ClusterRole, or each listed namespace's Role):

- **`incidents` in `kausa.fedeam.com`:** `create`, `list` and `delete`, only with incidents on.
- **`remediations` in `kurita.fedeam.com`:** `list` and `watch`, only with the report of point 6
  on. This amends "Never granted: any custom resource but kausa's own"
  ([Permissions](../reference/permissions.md)): kausa reads one resource of another project, and
  writes none.
- **Never:** `get`, `watch`, `patch`, `update` or `deletecollection` on incidents;
  `incidents/status`, until the outcome check adds `patch` there; any write on `remediations` or
  `remediations/status`; any verb on CustomResourceDefinitions. Secrets and ConfigMaps stay out in
  every verb.
- **Reader role:** with `incidents.readers.aggregate` (true by default, as for the history), the
  chart ships a ClusterRole `<full name>-incident-reader` with `get`, `list` and `watch` on
  `incidents`, labelled to aggregate to `view` only. The built-in `view` role carries the label that
  aggregates it to `edit`, and `edit` the one that aggregates it to `admin`, so those three read
  verbs reach `view`, `edit` and `admin`. Only read verbs are aggregated; no write verb on either
  resource is. kurita's chart does the same for `remediations`. The reader role is bound to nobody
  and grants kausa nothing.
- **The kind test** checks, with `kubectl auth can-i` as kausa's service account, the rows above
  with each switch on and off, and what each must refuse, `--subresource=status` included; and,
  for a subject bound to `view`, to `edit` and to `admin`, that each can `get`, `list` and `watch`
  incidents and can do nothing else to them. With the defaults the matrix is today's.

kurita's side (its ADR 0008, point 7): `list` and `watch` on `incidents`, never `get` and never a
write on `incidents` or `incidents/status`; `create`, `list` and `delete` on `remediations`.

### 6. Reporting a remediation

kurita records every outcome for an Incident in a `remediations.kurita.fedeam.com/v1alpha1`
object in the Incident's namespace, named `kurita.<fingerprint>.<firstSeenUnix>` after it; its
`spec` names the Incident (`incident.name`, `incident.uid`, `incident.detection`,
`incident.fingerprint`), the `target` (always the Incident's `object`), the `action` (`none`,
`needs-human`, `inspect`, `rollout-undo`, `delete-evicted-pods`, `raise-memory-limit` or
`set-image-pull-policy`), `mode: proposed` and `proposedAt` (kurita's ADR 0008, point 2).

With `incidents.reportRemediations` (off by default; it needs incidents and notifications on, and
`dryRun` false), kausa watches `remediations` and posts to its webhook what kurita proposed:

- **The lookup is in kausa's own memory**, the table of point 1: the Incidents kausa created in
  this process, at most 500, none dropped before it is 6 hours old, kurita's whole window to
  decide one. For a Remediation in
  namespace N named `kurita.<rest>`, kausa looks up `kausa.<rest>` in N in that table, and compares
  the UID the server gave its Incident with `incident.uid`. It reads no Incident: it has no `get`
  on them. This settles what kurita's ADR 0008 left to this ADR (its "Not verified").
  **A Remediation whose Incident is not in the table, is in another namespace, or has
  another UID is never reported** (`incident-unknown`). So a writer of Remediations in one
  namespace cannot make kausa report about another namespace's incident, which kurita relies on.
- **The checks**, all of them, before anything is sent: the record passes kausa's strict parse of
  the fields it reads, and its `incident.detection` and `incident.fingerprint` equal kausa's
  (`invalid`); the action id is one kausa knows (`unknown-action`) and one kurita's ADR 0008 allows
  for that detection (`invalid`); `target` equals the object kausa holds for the Incident
  (`target-mismatch`); `proposedAt` is within `[firstSeen, now]` (`time-out-of-range`); and no
  message was sent for that Incident before (`duplicate`).
- **kausa's copy of the actions each detection allows** is a table in kausa's source, a constant
  of the pure module that parses Remediations (named with the code), listed on [Data sent in
  notifications](../reference/data-sent-in-notifications.md) and pinned by a unit test: `none`
  and `needs-human` for every detection; `inspect` for every detection but `D009` and `D014`;
  `rollout-undo` for `D001` to `D005` and `D014`; `raise-memory-limit` for `D003`;
  `set-image-pull-policy` for `D002`; `delete-evicted-pods` for none. It equals the CEL rules of
  kurita's ADR 0008 (its point 2) and its `inspect.what` by detection. A new action id, or an
  existing id for one more detection, is sent only after a kausa change adds it to that table and
  to that page, under this repository's rule for what leaves the cluster; until then kausa counts
  it (`unknown-action` or `invalid`) and sends nothing.
- **`none` is never sent.** It means kurita found nothing to propose; `needs-human` and `inspect`
  are sent, since each tells a person something.
- **At most one message per Incident.** The table marks an Incident once its message is queued.
- **A record that fails a check is dropped**, counted in `kausa_remediations_ignored_total{reason}`
  (the six reasons above) and logged as `remediation ignored` through `log.rs` with its reason and
  its namespace and name, each checked against its pattern first, at most once a minute per reason
  (the others of that minute are counted, not logged). It is never retried, and no part of it is
  sent. **The counter counts evaluations, not records:** a relist of the watch evaluates every
  record again, so a record already sent or of an Incident not in the table counts once more
  (`duplicate`, `incident-unknown`) at each relist. Writes by others are not bounded (Costs), and
  the rate limit keeps them from flooding the log.
- **`remediation` messages have their own bound and never cost a `detected` or an `explained`
  message anything.** At most the value of `--notify-max-per-hour` of them start in any 60 minutes,
  counted apart from the `detected` ones, which never include them; no new flag. Over it a
  `remediation` message is not sent, counted and logged as `remediation capped` at most once an
  hour. At the default of 30, the webhook then carries in any 60 minutes at most 30 `detected`
  messages, 30 `remediation` messages and 1 `capped` message, plus the `explained` messages, which
  the call budget bounds (`--max-calls-per-hour`, 30 by default).
- **Their own counter.** `kausa_notifications_total` never counts a `remediation` message, in any
  outcome, so its `capped` and `dropped` keep meaning `detected` and `explained` messages
  ([Alerts](../reference/alerts.md)), and a writer of Remediations cannot inflate them. This ADR
  proposes `kausa_remediation_notifications_total{outcome}`, with the outcomes `sent`, `failed`,
  `dropped` and `capped` of the existing counter; the reference page fixes the name with the code.
  `kausa_notifications_retried_total` does not count them either: a `remediation` message's
  second attempt is not counted apart, only its final outcome.
- **Last in the queue, on the way in.** In the notification queue of 16 entries (`QUEUE_LIMIT`,
  `src/notify.rs:37`), when the queue is full, the oldest queued `remediation` is dropped first;
  with none queued, a new `remediation` is dropped itself; only then does the existing rule apply
  (the oldest `detected`, else a new `detected`, else the oldest `explained`,
  `src/notify.rs:478-494`). So a `remediation` never evicts a `detected` or an `explained`
  message, and either one evicts a queued `remediation`. A dropped `remediation` is counted and
  logged as `remediation dropped` at most once an hour.
- **Last in the queue, on the way out.** Today `take_next` takes messages in arrival order
  (`pop_front`, `src/notify.rs:524-559`), after the owed `capped` message when it is due. With the
  report it takes, after that `capped` message, the oldest queued message that is not a
  `remediation`, and the oldest `remediation` only when no `detected` and no `explained` message is
  queued. So a `detected` or an `explained` message waits for a `remediation` only when one is
  already being sent as it arrives: one send, bounded by one attempt of at most 10 seconds
  (`TIMEOUT`, `src/notify.rs:57`), the wait of 30 seconds before the one retry (`RETRY_WAIT`,
  `src/notify.rs:70`, which is `explain::RETRY_DELAY`, `src/explain.rs:107`) and a second attempt
  of at most 10 seconds: 10 + 30 + 10 = 50 seconds at most, and 1 second (3 with Telegram) when the
  receiver answers at once. The cost falls on the `remediation` messages, and is intended: they
  wait behind every `detected` and `explained` message, and under sustained load they go out late
  or are dropped by the rule above. What kurita proposed stays in its Remediation and in kurita's
  own Event either way.
- **Neither a dropped nor a capped `remediation` owes the `capped` message:** its fixed text speaks
  of incidents not posted and of kausa's log having every one (`src/notification.rs:170-175`), and
  a `remediation` is a notice about kurita, not an incident, so it would mislead; the code's queue
  does not force it, since only its own drops call `owe()`.
- **The `sent` check of `detected` messages does not apply to them.** `enqueue` and `take_next`
  drop a message with no `explanation` whose fingerprint `sent.holds` (`src/notify.rs:474`, `:539`),
  and an incident's `detected` message has nearly always been sent by the time kurita proposes,
  so a `remediation` queued as such an item would be dropped silently. A `remediation` message is
  its own kind of item, and only point 6's `duplicate` check applies to it. A test shows it: a
  `remediation` message for a fingerprint whose `detected` message was sent is sent.

**The message**, `type: remediation` within `v2` (0027's acceptance `Update`, its item "Within
`v2` a value of `type` may be added": a receiver must ignore a message whose `type` it does not
know):

```json
{
  "schema": "kausa.fedeam.com/notification/v2",
  "type": "remediation",
  "kausa_version": "0.8.0",
  "detection": {"id": "D003", "name": "out of memory"},
  "object": {"apiVersion": "apps/v1", "kind": "Deployment", "namespace": "shop", "name": "web"},
  "fingerprint": "5d0c2b7e9a1f4c38",
  "remediation": {"action": "raise-memory-limit", "result": "proposed",
                  "time": "2026-11-24T10:00:12Z"}
}
```

(invented values; the version is whichever release ships this). **The action id and the time come
from the Remediation**, after the checks; **the detection, the object and the fingerprint come
from kausa's own memory of the Incident**, never from the Remediation or the Incident read back.
`result` is always `proposed`, kausa's fixed word, since kurita v0.1 only proposes. Nothing else of
the Remediation leaves: not its reason, its parameters, its preconditions, its decider or any
name in it. The `slack` and `telegram` texts are one fixed line built from the same fields, such
as `kausa: kurita proposed raise-memory-limit for out of memory (D003) in shop, Deployment web`;
the reference page fixes it.

### 7. What kausa is

README's design principle "Read-only, except for its Events" reads, with incidents on, "and its
own Incidents", and, with the report on, kausa reads kurita's Remediations. kausa still changes no
workload, executes nothing, and never acts on a model's answer or on a Remediation: it only
reports one.

### 8. Invariants

Changed in [ARCHITECTURE.md](../../ARCHITECTURE.md) with the code:

- **Invariant 2** gains: "and, with incidents on, Incidents (`create`, and, for retention, a
  `delete` with UID and `resourceVersion` preconditions)".
- **Invariant 5** gains: "Nothing read back from an Incident or a Remediation is logged, sent or
  reused, except a Remediation's action id and `proposedAt`, which, after the checks of 0028, are
  sent as a `remediation` message's action and time, and the namespace and name of a dropped
  Remediation, logged after their patterns are checked." The `metadata.uid` of the answer to kausa's
  own create is the stated exception to that text: it is parsed as a UID and kept, as 16 bytes, only
  in the table of created Incidents (point 1), never logged, sent or reused otherwise; the rest of
  the answer is read under a cap of 64 KiB and dropped. An error answer is not under that cap:
  kube-rs reads its body whole before kausa sees it, bounded only by the 10 seconds, as for every
  request kausa makes, and kausa keeps only its code and reason.
- **Invariant 11** gains the `remediation` message and its fields.
- **Invariant 12** gains: "An Incident holds only what a fact statement may hold and the typed
  values of 0028's schema; changing what it may hold needs an ADR."
- **A test checks that nothing from either resource reaches `context.rs`**, so nothing new goes to
  the model (invariant 3 unchanged).

### 9. Attach point of the outcome check

A later ADR of kausa's checks whether a proposed remediation worked. It attaches at
`Incident.status`, declared empty here, and adds kausa's `patch` on `incidents/status` itself; it
is what moves `incidents.days` if a day is too short. kurita's execution, if an ADR ever decides
one, attaches at `Remediation.status`, which kausa never writes. This ADR decides neither.

### 10. Without kurita, and out of step

- **kausa with its defaults** creates no Incident and reads no Remediation; it behaves as today.
- **Incidents on, no kurita:** kausa creates and sweeps its Incidents, and nothing reads them but
  whoever has `view`.
- **Report on, no Remediation resource installed** (or another version): kausa logs `remediation
  source missing` at most once an hour, sets a gauge to 0 (this ADR proposes
  `kausa_remediation_source_ready`; the reference page fixes it), retries, and stays Ready.
- **Within `v1alpha1` changes are additive only;** each side parses strictly and counts what it
  ignores.
- **An action id kausa does not know** is counted as `unknown-action` and not sent, so kurita can
  ship a new id without breaking kausa; kausa sends it only once a kausa change adds it to its
  table of point 6.
- **After a restart or a failover** the table is empty, and it is never seeded from a list of
  Incidents (invariant 5): Remediations of earlier Incidents are not reported (`incident-unknown`),
  the new process may create 500 Incidents more (point 4), and kausa may create a second Incident
  for the same fingerprint, under a new first-seen time;
  kurita's budget per fingerprint absorbs it (`needs-human{cooldown}`) when the first Incident's
  record counts in that window, and otherwise kurita decides the second like any other Incident
  (point 1). With `--leader-lease`, only the holder creates, sweeps and reports.

### Trust statement

"Among principals with no wildcard or cluster-admin grant, the charts give write verbs on
`incidents` only to kausa's service account and on `remediations` only to kurita's."

The others who can write either resource in a namespace N:

- cluster admins, and holders of roles with `*` resources or verbs, or with `*/status`;
- holders of cluster-admin, or of a wildcard role, through a RoleBinding in N;
- anyone who can run Pods as either service account, impersonate it, or mint tokens for it, which
  includes `edit` in that controller's namespace;
- whoever can update the CustomResourceDefinitions.

The schema bounds the `spec` for every writer. It does not bound `metadata`, a delete followed by
a new create, or a create with any valid `spec`.

- **A writer of Incidents can:** make kurita check the live objects again, in N only; spend kurita's
  proposal budget; suppress an undo proposal, or let one through, by forging the revisions of point
  3; push the sweep of point 4 past its target, so that it deletes older Incidents of every
  namespace, younger than 6 hours included; make kausa send nothing about it, since kausa reports
  only from what it holds in memory; and take an incident's name first: it creates
  `kausa.<fingerprint>.<firstSeenUnix>` before kausa does, so kausa's create fails, is logged and
  counted and is not retried (point 1), and kausa holds nothing and reports nothing about that
  incident.
- **A writer of Remediations in N can:** make kausa send one false "kurita proposed X", where X is
  an action id allowed for a real incident kausa detected in N, at most once per Incident and
  within the `remediation` bound of point 6, which no `detected` or `explained` message shares;
  and take kurita's claim first, so that kurita's create answers `AlreadyExists` and kurita
  proposes nothing for that Incident, unable to tell that the record is not its own. Outside N, it
  can spend kurita's cluster budget, silence kurita by filling kurita's cap, or, by deleting
  records, let kurita propose more than its budget allows in an hour (kurita's ADR 0008, its trust
  statement); none of that makes kausa send anything.

**A writer of Pods or ReplicaSets in N**, which the built-in `edit` role in N is, writes no Incident
but chooses what point 3 reads: a Pod's controller `ownerReference` names the ReplicaSet the fact
slot reads, and that ReplicaSet's `deployment.kubernetes.io/revision` annotation is the revision it
copies. So the writer can make an Incident in N about a failing Pod carry a revision of its choice,
or none (a ReplicaSet that does not exist, an annotation that is not a whole number in 1..2^53, or a
reference that pairs one ReplicaSet's name with another's UID gives `revision-lookup-failed`), and
so suppress kurita's undo proposal for a Deployment of N, or let one through, as a writer of
Incidents can (above), on workloads it can already change itself. With a controller `ownerReference`
whose apiVersion, kind, name or UID does not fit the Incident's schema, on a failing Pod named to
sort first, it can make kausa create no Incident, at no request (point 2's rule), for an incident of
any workload of N whose sample is that Pod, also one whose other Pods it does not write: an
incident's key is its namespace, its owner's kind and name and its detection (`src/incident.rs:33`,
`:256-263`), its sample the first signal by Pod, container, reason and message (`:477-489`), and its
`object` the sample's owner (`:492`); a reference naming the real Deployment, which kausa does not
hop from (`src/owner.rs:22-28`, `:61-62`), or the real ReplicaSet, which the lookup resolves by name
to its Deployment while the reference's UID stays the sample's `controller.uid`, puts the Pod in
that incident. That the API server accepts such a reference, and how long its garbage collector
leaves the Pod, are under "Not verified". What is seen then: kurita gets no Incident and proposes
nothing, not even `inspect`; the reader of notifications gets the `detected` message and never
"kurita proposed"; and the refusal is counted and logged with the reason `schema` (point 1). Nothing
outside N, no request and no memory. What is new is small: with a false but well-formed UID the same
writer already got `none{target-replaced}` from kurita (below). A name that is not a DNS-1123
subdomain of at most 253 characters gets it nothing more: no request and no cache entry (point 3).
What it costs kausa: the read is made only for a create kausa will send, once per create attempted,
at most 500 `get`s in any 6 hours for creates the server accepts (point 3 bounds the failed ones;
under point 2's rule no value this writer chooses reaches the server and fails there, so they come
from a writer of Incidents, which can take an Incident's name first, or from the API server), each
holding the fact slot at most 5 seconds, by which the next facts' Events wait. Outside N, that wait
is all it gets: the incident's own `Detected` Event is written before the read, and the facts of
other namespaces wait at most 5 seconds per create attempted, which for the creates the server
accepts is 500 × 5 = 2,500 seconds in any 6 hours (11.6 % of 21,600), and only while the API server
answers those `get`s slowly. That the writer cannot lengthen a metadata `get` by itself, and that
the wait does not fill the fact queue for other namespaces, are under "Not verified".

**A writer of Deployments in N**, which `edit` is, writes no Incident, but can set its Deployment's
`deployment.kubernetes.io/revision` annotation to 0 or past 2^53, which the fact statement parses as
any `u64` (`src/fact.rs:134-138`), so that the `D014` of that Deployment gets no Incident, at no
request (point 2's rule). What is seen is as for the writer of Pods above, about a Deployment it
already writes.

**Whoever can create Events in N**, which the built-in `edit` role in N can, writes no Incident but
chooses, through kausa, what some of kausa's Incidents in N say. For an Event incident kausa takes
the fingerprint from the Event's note (`src/fingerprint.rs:63-77`), so the writer chooses as many
fingerprints as it likes, and:

- for `D008`, `D011`, `D012` and `D013`, whose Event is about something other than a Pod, the
  Incident's `object`, its UID included, comes from the Event's `regarding`
  (`src/incident.rs:181-192`), except that for a Job or a ReplicaSet kausa reads with an owner,
  the `object` is that owner as kausa reads it (`src/run.rs:1528-1550`); so the writer chooses the
  object, existing or not, and its UID;
- for `D006`, `D007` and `D010`, whose Event is about a Pod, the `object` is the owner kausa holds
  for a Pod of N it has seen (`src/incident.rs:194-198`), so the writer chooses which one.

With a `regarding` whose `uid` or `apiVersion` does not fit the schema, naming the object of an
incident of a genuine Event, an Event with the same reason and an empty note falls in that
incident's key and sorts first: kausa copies the `regarding` (`src/run.rs:1534-1539`), keeps it as
the owner when it is not a kind it hops from (`src/owner.rs:61-62`) and otherwise resolves it by
name, while the Incident's `regarding` keeps the Event's values, and sorts an empty message first
(`src/incident.rs:477-489`). So kausa creates no Incident for that incident, also when its other
Events are genuine, at no request. What is seen is as for the writer of Pods above. That the API
server accepts such an Event is under "Not verified".

What that achieves: Incidents in N with those objects, each making kurita record an outcome, which
spends kurita's budget for its namespace and fingerprint, and the cluster's, only if that outcome
counts (kurita's ADR 0008, point 8), which is when it is an `inspect` about a failing object
kurita's live read confirmed, and each taking a place in the table of point 1, so that 500 of them
in 6 hours stop new Incidents in every namespace until the oldest of them passes 6 hours, and for as
long as the writer goes on: one Event with a new fingerprint every 6 × 3,600 / 500 = 43.2 seconds
holds the stop (point 4); and, for the outcomes kurita gives without a live read that confirms the
failure, a sent `remediation` message "kurita proposed needs-human" about an object the writer chose
in N. Those outcomes are `needs-human{unconfirmable}`, which kurita gives without a live read that
confirms the failure: before reading anything when it cannot identify the object, and after reading
it when no typed field can confirm the failure (kurita's ADR 0006, its point 5, and its ADR 0008,
point 6); kurita's ADR 0008 lists the cases in its trust statement, and this ADR does not repeat the
list. For kausa it means a `needs-human` message that kausa sends about an object the writer chose
in N, within the bounds below. Among the cases an Event can reach: `D006` when no container with a
readiness probe is running and not ready; `D011`, `D012` or `D013` whose `object` is of a kind or an
apiVersion kurita does not recognise; `D012` and `D013` whose `object` has no UID, which happens
whenever the Event's `regarding` has none, since kausa then leaves the UID empty
(`src/incident.rs:186-191`); and `D008` whose Job is its own owner and has no UID, for the same
reason. Every other outcome comes after a live read of the object: one of an object that does not
fail gives `none{not-reproduced}`, which is not sent, and a forged UID gives
`none{target-replaced}`. What it cannot: an Event alone never yields an acting id or a proposal of
an action that changes an object, since kurita records an acting id only after a live read confirms
the target's UID (its ADR 0008, point 2); what it chooses, the objects and the sent "kurita proposed
needs-human" or "kurita proposed inspect", is in N only. For an object of N that really fails, a new
fingerprint for each forged Event sidesteps kurita's per-fingerprint window, which leaves several
true, repeated "kurita proposed inspect" messages about it, within point 6's `remediation` bound.
Outside N it has three effects, all towards silence: the stop of point 4, no new Incidents in any
namespace, and so no proposals of kurita, for as long as it goes on; and the `remediation` bound of
point 6, which is one for the process: 30 Incidents an hour whose outcome kurita gives before any
live read, one forged Event every 3,600 / 30 = 120 seconds, 30 an hour, well under the 83.3 an hour,
3,600 / 43.2, that hold the stop, fill it with sent "kurita proposed needs-human", and kausa then
sends no `remediation` message about kurita's genuine proposals in any namespace while the writer
keeps that rate or a little above it (counted and logged as capped; each proposal stays in its
Remediation and in kurita's Event, and nothing is executed); and, bounded, kurita's cluster window:
the `inspect` outcomes on failures kurita's live read confirms count in it too, at most kurita's
per-namespace limit an hour from N, so filling it takes that in as many namespaces as the ratio of
the two limits, and genuine Incidents in every namespace then get `needs-human{budget}` (kurita's
ADR 0008, point 8), while the `needs-human` outcomes kurita gives before a live read count in none
of its windows. kausa still writes its Events and posts its `detected` messages. It is bounded by
the create gate of point 1, by the 500 Incidents in any 6 hours of point 1 and by the `remediation`
bound of point 6.

**What the create gate's key admits.** The gate of point 1 is keyed by namespace and fingerprint,
as kurita keys its budget, and that is decided. The fingerprint is not collision resistant: a
writer of Events in N who can also read N's Incidents can get an Incident created in N whose
fingerprint equals a genuine incident's, and the genuine occurrence is then a duplicate for
`--dedup-hours`, so kausa creates no Incident for it, and no refusal is counted. It reaches N only,
and 0027's dedup of notifications has the same property. It is not demonstrated, since no collision
was computed, and the key stays.

**Already true before this contract, and out of its scope:** editors of kausa's namespace can read
the webhook's Secret (`charts/kausa/values.yaml:96-99`) and post anything to the receiver
directly; and anyone who can create Events can make kausa detect a forged Event incident and post
its `detected` message (0027). What this contract adds to that is the paragraph above.

Checked in the Kubernetes documentation and source while this was designed: `edit` can create
Events; `edit` can impersonate service accounts and create their tokens; a `*/status` rule reaches
the `status` subresource of every resource, and a rule on `incidents` alone does not. Checked in
the source only, not on a cluster: a write through `/status` cannot change `metadata` or `spec`,
and rules at the root of a CRD's schema run on `/status` writes.

### Costs, computed

Where a figure is an estimate carried from the design review and not derived here, it says so;
bar 2, 3 or 6 measures it.

- **The fact queue, incidents off:** +8 bytes an entry, 8 KiB full. The extra data sits behind one
  optional pointer, 8 bytes on a 64-bit target: 8 × 1024 = 8,192 bytes. So 1,344 bytes an entry
  and 1,376,256 bytes (1.31 MiB) full: (1,336 + 8) × 1024.
- **The fact queue, incidents on, worst case:** about +320 bytes an entry, an estimate carried from
  the design review (dominated by a `regarding` name of up to 253 bytes, a bound that point 2's rule
  and the lookup's of point 3 make true; the UIDs kept as 16 binary bytes), not derived field by
  field here; bar 2 measures it. The ReplicaSet's name of point 3 shares that field, since an entry
  holds a `sample` or a `regarding`, never both, and adds nothing; kept in a field of its own it
  would add an empty string's 24 bytes to an entry with a `regarding`, (1,336 + 344) × 1024 =
  1,720,320 bytes = 1.641 MiB computed, 1.85 to 1.95 MiB resident at the ratios below, still under
  the bar. Then (1,336 + 320) × 1024 = 1,695,744 bytes = 1.617 MiB computed, and about 1.82 to 1.92
  MiB resident, at the ratio of measured to computed in 0027's prototype. The queue computed there
  is 1,583 × 1024 = 1,620,992 bytes = 1.5459 MiB (1,620,992 / 1,048,576). In kind the resident
  memory grew by 1.74 MiB for the queue alone, a ratio of 1.74 / 1.5459 = 1.126; the control group's
  anonymous memory grew by 1.80, 1.80 and 1.84 MiB for the queue and the table of notified
  fingerprints together, and the highest reading gives 1.84 / 1.5459 = 1.190 ([The fact statement
  prototype, measured](../research/2026-11-20-fact-statement-prototype.md)). So 1.617 × 1.126 = 1.82
  MiB and 1.617 × 1.190 = 1.92 MiB, under the bar of 2 MiB by 0.08 MiB at the highest reading, which
  counts the table (40 KiB computed) as if it were the queue's and so errs high. The bar is not
  raised. On the Raspberry Pis the prototype did not measure the queue alone. Update 2026-11-25:
  measured on a 64-bit target, the longest entry holds 1,685 bytes, +349 over `4b85369`'s 1,336:
  the pointer's 8, an `IncidentData` of 88 and a ReplicaSet's name of 253, its longest. So a full
  queue holds 1,685 × 1024 = 1,725,440 bytes = 1.6455 MiB computed, and 1.6455 × 1.126 = 1.853 MiB
  and 1.6455 × 1.190 = 1.958 MiB resident at the ratios above, under the bar by 0.04 MiB at the
  highest reading. Off, an entry holds 1,344 bytes and a full queue 1,376,256, as computed
  (`a_full_queue_of_the_longest_entries_is_within_the_computed_bound` in `src/report.rs`).
- **The table of created Incidents:** about 48 bytes an entry without the report and at most about
  750 bytes with it, estimates carried from the design review, not derived here; bar 3 measures
  them. Then 500 × 48 = 24,000 bytes, about 23 KiB, and 500 × 750 = 375,000 bytes, 0.36 MiB. The
  rule of point 1 keeps a creation time in each entry, a monotonic instant of 16 bytes on a 64-bit
  Linux target; whether the review's estimates count it is not known, so at most 500 × 16 = 8,000
  bytes more: 383,000 bytes, 0.37 MiB, with the report. Bar 3 measures the whole entry. Update
  2026-11-24: measured on a 64-bit target, an entry holds a slot of 152 bytes and at most 632 bytes
  of names on the heap, 784 bytes, and a full table of the longest entries 500 × 784 = 392,000
  bytes, 0.37 MiB, under bar 3's 524,288 bytes, which measures them
  (`a_full_table_of_the_longest_entries_is_within_bar_3` in `src/incident_resource/table.rs`).
- **The owner cache:** +8 bytes an entry, one optional pointer, empty with incidents off (bar 1);
  with incidents on, 24 bytes more for each entry of a ReplicaSet whose `get` answered, the revision
  (8) and the UID its `get` returned (16), plus the allocator's overhead, not counted. The name in
  an entry's key is at most 253 bytes under the lookup's rule (point 3); at `4b85369` it has no
  bound. That rule bounds the cache's key, not the name itself: an `ownerReference`'s or an Event's
  `regarding`'s name of any length still enters 0027's fact queue as the incident's object, and its
  `incident` log line, as it does at `4b85369`; this ADR's part of the entry holds only values that
  fit (point 2).
- **The sample's UID in each signal and incident:** a signal about a Pod carries its record's UID
  (point 3), with incidents off too, since detection does not read the option: measured on a 64-bit
  target, a `Signal` grows from 392 to 416 bytes and an `Incident` from 648 to 672, and a signal
  about a Pod clones the UID's 36 bytes to the heap. An incident is held only while its tick handles
  it and in an explanation's work: queued (at most `explain::QUEUE_LIMIT`, 16), running, or waiting
  for its retry. No entry of the fact queue grows: a fact does not keep the signal.
- **The refusals' counter and log line:** three series of `kausa_incidents_refused_total`, one per
  reason, and three hour marks, one for each reason's line, a monotonic instant of 16 bytes each on
  a 64-bit Linux target (point 1).
- **The table of Events:** one flag an entry, whether the owner lookup of the Event's object failed
  (point 3's `missing`), kept with incidents off too: at most 8 bytes an entry with alignment, an
  estimate; bar 1 measures it. Update 2026-11-25: measured on a 64-bit target, an entry grows
  from 368 to 376 bytes.
- **Reads:** at most one metadata `get` of `replicasets` per create attempted, when the sample's
  owner-cache entry is absent or expired at the read: at most 500 in any 6 hours per process for
  creates the server accepts, 0.023 a second at the most, and, for failed creates, at most one per
  ReplicaSet name every 600 seconds while its `get` answers or every 30 seconds while it fails
  (point 3), times the ReplicaSets whose Pods fail, names a writer of Pods in N chooses. Each holds
  the fact slot at most 5 seconds: 2,500 seconds in any 6 hours for the 500, 11.6 % of the slot's
  time, only while the API server answers slowly. None with incidents off.
- **Writes per incident:** kausa +1 create and, later, +1 delete; kurita +1 create and +1 delete,
  besides its own Event. **Writes per change of an object:** 0; neither resource is patched.
- **Stored objects:** an Incident at most about 2.5 KB (a `spec` of about 1.5 KB at the longest
  names, plus metadata) and a Remediation at most about 7 KB, estimates carried from the design
  review; bar 6 measures them with `managedFields`. At 500 of each: 500 × 2.5 KB = 1.25 MB of
  Incidents and 500 × 7 KB = 3.5 MB of Remediations. **At kausa's worst case between sweeps**
  (point 4), 1,000 Incidents: 1,000 × 2.5 KB = 2.5 MB; each process start whose sweep does not
  complete can add 500 × 2.5 KB = 1.25 MB more, until one does.
- **Watches:** kausa 1 on `remediations`, across the cluster or 1 per listed namespace, only with
  the report on; kurita 1 on `incidents`, or 1 per listed namespace. kurita's share of the events
  on kausa's watch is one create and one delete per Incident kausa creates, for every outcome, so
  it follows kausa's rate of Incidents, which kausa's create gate (one per fingerprint per
  `--dedup-hours`) and its table (at most 500 creates in any 6 hours per process, point 1)
  bound. kurita never deletes a Remediation younger than 6 hours and creates none while 500 exist,
  so within one kurita term kausa's watch sees at most 500 creates by kurita in any 6 hours, plus
  the deletes of older records. Across terms it can see more: kurita counts from the list it reads
  at the start of a term (its ADR 0008, point 8), so records others delete let the next term
  create more. Writes to `remediations` by anyone
  else are not bounded by this ADR or by kurita's.
- **The memory of the `remediations` watch:** it starts from kausa's `watcher_config()`, as every
  watch of kausa's does, so its initial list and each relist ask for pages of 50
  (`LIST_PAGE_SIZE`, `src/run.rs:94`, used at `src/run.rs:395`), and kube decodes a whole page at
  once. The schema bounds the `spec`, not `metadata`: annotations reach 256 KiB, and an object the
  API server's limit, about 1.5 MiB in etcd. One object near that limit took about 12 MiB once
  parsed (0014's `Update 2026-10-28`), so the worst transient page, computed, is 50 × 12 MiB =
  600 MiB, past the chart's limit of 192Mi (`charts/kausa/values.yaml:155`): a writer of
  Remediations, within the trust statement, could make kausa restart at each relist. The Events
  watch has the same exposure today, to anyone who can create Events, and so do the Pod and
  Deployment watches to their writers (`src/run.rs:638-647`; [List pages](../research/2026-11-02-list-pages.md),
  its "Limits"); this watch adds writers of Remediations to them. The page stays 50. Pages of 2,
  as 0014's sweep reads, would bound this watch's page to 2 × 12 MiB = 24 MiB at about 500 / 2 =
  250 list requests per relist of 500 records, but would remove nothing: whoever can write
  Remediations, within the trust statement, can also create Events, and the Events watch's pages
  stay 50. "Not verified" lists the measurement.
- **Write latency, for reference:** 7.7 to 21 ms a write, **computed** from Event totals, not
  measured for these resources: 1024 `Detected` Events took 7,906 to 21,487 ms in all on three
  Raspberry Pi 4 Model B (0027's research note), and 7,906 / 1024 = 7.7 ms and 21,487 / 1024 =
  21.0 ms. The slowest run patched existing Events rather than creating them.

### Conditions of acceptance

Measured on three Raspberry Pi 4 Model B (8 GB, k3s, about 100 Pods) and in kind with 10,000 Pods,
before this ADR and kurita's ADR 0008 are accepted:

1. With incidents off: kausa's idle memory within the run-to-run spread of its previous release,
   and the `kubectl auth can-i` matrix unchanged.
2. A full fact queue at the longest names adds at most 2 MiB (0027's point 10, unchanged).
3. The table of created Incidents at 500 entries is at most 0.5 MiB.
4. 1024 `Detected` Events and 1024 Incident creates in a row: no watch error, relist or restart in
   kausa or kurita; the drain time recorded against the 7.9 to 21.5 seconds of Events alone, with
   the `get`s of point 3's read counted and the time they held the slot, and the fact queue's
   length recorded while they ran.
5. A sweep of 1,000 Incidents, kausa's worst case between two sweeps (point 4), deletes the 500
   beyond the cap within its 30 seconds and causes no relist.
6. The stored sizes of both resources, `managedFields` included, on k3s with its default
   datastore, recorded against the estimates of 2.5 KB and 7 KB above. Observational: it does not
   condition acceptance by itself, and a size over the estimate amends the costs.
7. Both CustomResourceDefinitions are accepted within the CEL cost budget on the oldest supported
   k3s.
8. More than 500 fingerprints settle inside an hour, in kind: creates stop at 500, each refusal is
   counted, the incidents' Events and `detected` messages are all written and posted (within
   `--notify-max-per-hour`), and after a restart that adds 500 more, one sweep returns the count
   to 500 inside its 30 seconds.

kurita's ADR 0008 adds two bars of its own (its 8 and 9, in its own numbering), on its relist and
its cap; the two ADRs are accepted together. If bar 2 fails after the trims of point 3, the
maintainer chooses between dropping `regarding.name`, so that kurita gives
`needs-human{unconfirmable}` again for `D008` with a controller and for `D011`, and changing
0027's point 10; both ADRs are amended accordingly.

### Not verified

- **Whether the metadata `get` of a ReplicaSet returns its annotations,** where the revision is.
  Test: read a ReplicaSet with a metadata-only `get` in kind and check the
  `deployment.kubernetes.io/revision` annotation.
- **That the API server asks an ownerReference's `uid` only to be non-empty,** on which point 2's
  rule rests for `object.uid` and `sample.controller.uid` (a hypothesis, not read in its source),
  and how long the garbage collector takes to delete a Pod whose owner's UID matches no object.
  Test: in kind, create a Pod with `ownerReferences[0].uid: x` naming a Deployment of N, check that
  it is accepted, and time its deletion.
- **What the API server does today with `..` or `?` in a looked-up name,** before the lookup's rule
  of point 3, and whether it asks an ownerReference's name only to be non-empty (a reviewer's
  reading of `apimachinery/pkg/api/validation/objectmeta.go:79-80`, not re-read here). Test: in
  kind, an authenticated `curl` of
  `/apis/apps/v1/namespaces/N/replicasets/../../../namespaces/M/replicasets/x` with kausa's
  ServiceAccount token, recording the status and which object, if any, it returns; and a Pod whose
  `ownerReferences[0].name` is longer than 253 characters, recording whether it is accepted.
- **How often the sample's owner-cache entry is absent or expired at the read,** so how many `get`s
  point 3's read makes and for how long it holds the fact slot. Computed only as a bound, one per
  create attempted. Test: bar 4, and an Incident about a Pod older than 10 minutes in kind, whose
  `sample.controller.revision` must be set.
- **That a ReplicaSet's revision only grows, and that a Deployment's revision is never lower than
  any of its ReplicaSets',** on which point 3 rests the claim that a stale revision of the same
  ReplicaSet can stop an undo proposal and never allow one: a hypothesis from the Deployment
  controller's known behaviour (a rollback gives the reused ReplicaSet the highest revision plus
  one), not read in its source here. A ReplicaSet recreated under the same name is covered by the
  UID, not by this. Test: read the controller's source; in kind, roll a Deployment out and back and
  compare the revisions; and delete the sample's ReplicaSet with `--cascade=orphan` so that the
  Deployment recreates it under the same name and its Pods are adopted, then let an incident about
  one of them be created within 600 seconds of a cached read: its Incident must carry
  `revision-lookup-failed`.
- **Whether a rollback onto the sample's ReplicaSet between the tick and the read reaches an undo
  proposal in kurita,** point 3's "The revision at the read": kurita's point 4 passes; whether its
  rows' other rules then propose the undo was not walked here. Test: in kind, with kurita, roll a
  Deployment back onto the ReplicaSet of failing Pods while their fact waits in the queue.
- **That a writer of Pods or ReplicaSets cannot lengthen a metadata `get` by itself,** for instance
  with a ReplicaSet of about 1.5 MiB of metadata on a Raspberry Pi; the exposure exists today in
  `on_pod`. Test: bar 4 with such a ReplicaSet as the sample's, recording each `get`'s time.
- **That the reads' wait, added to the `Detected` Event's 10 seconds and the create's 10, does not
  fill the fact queue (1,024) for other namespaces** under point 3's bound. Test: bar 4, with the
  reads' `get`s slowed.
- **That one forged Incident every 120 seconds exactly leaves no gap** in which a genuine
  `remediation` message is sent: point 6's bound counts over a rolling 60 minutes, and whether a
  message 3,600 seconds after the first of 30 counts in the window or out of it depends on the
  bound's code, which is not written. Hence "at that rate or a little above it" in the trust
  statement and Consequences. Test: read it against the bound's code once it exists, with a unit
  test at exactly 120 seconds.
- **The memory of a relist of the `remediations` watch with objects near the size limit** (Costs:
  600 MiB computed for a page of 50). Test: in kind, create 50 Remediations with about 1.5 MiB of
  `metadata` each, force a relist, and read kausa's peak memory; do the same with 50 such Warning
  Events, the exposure that exists today.
- **That one sweep deletes 500 Incidents within 30 seconds in pages of 2:** 0014 gives no time per
  request. Computed: listing 1,000 objects in pages of 2 is 500 list requests, and 500 deletes at
  7.7 to 21 ms each take 500 × 7.7 = 3,850 ms to 500 × 21 = 10,500 ms, which leaves 30 − 10.5 =
  19.5 s to 30 − 3.85 = 26.15 s for the 500 lists, 39 to 52 ms each, a list latency not measured
  anywhere here. Test: bars 5 and 8. A sweep that stops at its deadline still deletes as it
  goes: past the first 500 objects it has listed, each one more pushes the oldest it holds out of
  its heap, and that one is deleted at once (`src/history/pure.rs:491-545`, as 0014's). So a
  sweep that lists k objects deletes k − 500 by the cap, and the count still comes down over the
  hourly sweeps if each lists more than 500 + 84 = 584, since one process creates at most
  500 / 6 = 83.3 Incidents an hour on average over 6 hours (point 1); not measured either.
- **That a `/status` write cannot change `metadata` or `spec`, and that root CEL rules run on
  `/status` writes:** checked in the source only. Test both in kind before the outcome check
  relies on them.
- **The CEL cost of the per-detection rules and of `sample`'s rule.** Test: install both
  CustomResourceDefinitions on the oldest supported k3s (bar 7).
- **That a forged Event yields an `inspect` that kurita's cluster window counts:** test in kind that
  a forged `D006` or `D012` about an object that really fails gives `inspect`, and that kurita's
  cluster window counts it in the same term.
- **Nothing above is measured.** The Event latency is computed from measured totals.
- **That the API server accepts an Event whose `regarding.uid` is `x`,** on which the Events
  writer's paragraph of the trust statement rests. Test: in kind, create an Event with
  `regarding.uid: x` and the reason of a real incident's Event, about the same object, and check
  that it is accepted.
- **Whether `object.kind`'s pattern `^[A-Z][A-Za-z0-9]*$` admits every real kind.** A recollection
  from the ninth review, not read in source: the validation of a CustomResourceDefinition's names
  asks only that its kind, lowercased, be a DNS-1035 label, which would admit a kind that starts in
  lower case or holds `-`. Meanwhile an incident whose top owner has such a kind gets no Incident,
  counted and logged with the reason `schema` (point 2's rule), and its Events, its log line and its
  `detected` message are not affected. Test: read `ValidateCustomResourceDefinitionNames` in the
  apiextensions source, or, in kind, create a CustomResourceDefinition with `kind: fooBar` and a
  failing Pod, or a ReplicaSet, whose controller `ownerReference` is an object of it; a Deployment
  it owns would not do, since kausa follows only a ReplicaSet's and a Job's controller
  (`src/owner.rs:22-28`).
- **Which Pod an Incident describes while two records of one name are held,** the old one of a Pod
  deleted in a watch gap beside its replacement until the relist ends: the sample's Pod is found
  by UID, but `spec.facts.podsNow` counts both records (`pods_of`, `src/run.rs`), and the owner of
  an Event about a Pod, which becomes `spec.object`, is looked up by name and can come from the old
  record (`src/incident.rs:88-93` and `:194`, older than this ADR, as the fact statement reads
  them). Test: in kind, delete a crashing Pod during a watch gap and compare its Incident with the
  Pods.
- **Whether the API server can store an Incident and still give kausa no valid answer,** point 1's
  one exception to "every Incident it created in the last 6 hours is still in the table": an answer
  lost or cut after the object was stored. Only kausa's side is tested, against a fake server.
  Test: in kind, delay the create's answer past the 10 seconds with a proxy, then check that the
  Incident exists, that no entry was kept, and that the next occurrence creates a second one.
- **Whether the Incident should take as its sample the first signal of the group whose values fit,**
  and refuse only when none fits, so that a signal a writer in N places first does not leave a
  genuine incident without an Incident (the trust statement). Open for the implementation; the
  sample is not chosen differently here. Its cost: the Incident's sample could then differ from the
  one 0027's fact statement describes.

## Consequences

- **Another controller can read kausa's incidents as typed data**, from a resource only kausa's
  service account and the principals of the trust statement can write, without kausa reading,
  writing or executing anything more on workloads.
- **More writes, only when turned on:** one create and one delete per incident, and, with the
  report, one watch and one message per proposal. With the defaults nothing changes.
- **kausa reads another project's resource** for the first time, `remediations`, and writes none.
  Every field of it that leaves is typed and checked; a writer of Remediations gains one false
  notice per Incident at most, within the `remediation` bound, which never costs a `detected` or
  an `explained` message.
- **A writer of Events in a namespace gains, besides the forged detection it had,** Incidents about
  objects of its choice there and, for the outcomes kurita gives without a live confirmation, a sent
  "kurita proposed needs-human" about them, and, for an object there that really fails, repeated
  true "kurita proposed inspect" messages, in that namespace only, and never an acting id. Outside
  it, three effects, all towards silence: by the stop of point 4, no new Incidents in any namespace,
  and so no proposals of kurita, for as long as it goes on; and, since the `remediation` bound of
  point 6 is one for the process, one such Incident every 120 seconds (30 an hour), or a little more
  often, leaves no `remediation` message for kurita's genuine proposals in any namespace while it
  goes on, counted as capped; and, bounded, its `inspect` outcomes on failures kurita's live read
  confirms count in kurita's cluster window, at most kurita's per-namespace limit an hour from that
  namespace, so as many such namespaces as the ratio of kurita's two limits fill it and genuine
  Incidents everywhere get `needs-human{budget}` (kurita's ADR 0008, point 8). kausa still writes
  its Events and posts its `detected` messages.
- **A restart loses notices, never invents them:** Remediations of Incidents created before it are
  not reported.
- **The bounds are not fair:** whoever makes kausa create 500 Incidents in 6 hours, one tenant's
  volume or a writer of Events, stops new Incidents in every namespace until the oldest is 6 hours
  old, and for as long as it keeps up one create every 43.2 seconds; and a writer of Incidents
  can make the sweep delete other namespaces' Incidents before kurita decides them; both move
  kurita towards not proposing, and kausa's own Events and `detected` messages are unaffected.
- **A second Incident per fingerprint is possible** inside `--dedup-hours` when it is over 6 and
  the table had to drop the entry, as after a restart; kurita's budget per fingerprint absorbs
  it when the first Incident's record counts in that window, and otherwise kurita decides the
  second like any other Incident.
- **Pages that change with the implementation**, not with this ADR: a new reference page for the
  Incident resource, `docs/reference/incident-resource.md`;
  [Permissions](../reference/permissions.md), [Helm chart](../reference/helm-chart.md)
  (`incidents.enabled`, `incidents.days`, `incidents.installCRD`,
  `incidents.readers.aggregate`, `incidents.reportRemediations`), [Data sent in
  notifications](../reference/data-sent-in-notifications.md), [Command line](../reference/command-line.md)
  (the flags, log lines and metrics, `kausa_remediation_notifications_total` among them, and the
  meaning of `kausa_notifications_total`, which counts no `remediation` message),
  [Alerts](../reference/alerts.md) and [Detection and redaction rules](../reference/rules.md) (what
  `kausa_notifications_total{outcome="capped"}` and `{outcome="dropped"}` count),
  [Send notifications to Slack, Telegram or a webhook](../how-to/send-notifications.md) (the
  `remediation` message, its bound, its counter and its place in the queue),
  [ARCHITECTURE.md](../../ARCHITECTURE.md) (invariants 2, 5, 11 and 12), the design principle of
  [README.md](../../README.md), the [Security policy](../../SECURITY.md),
  [Architecture](../explanation/architecture.md) (its read-only principle),
  [AGENTS.md](../../AGENTS.md) (what kausa writes and reads), the introduction of [the
  documentation front page](../index.md), [Install kausa with Helm](../how-to/install-with-helm.md),
  [Set up a development environment and run the checks](../how-to/develop.md) and
  [CHANGELOG.md](../../CHANGELOG.md); the
  chart's `values.yaml`, `templates/_helpers.tpl`, `templates/incident-crd.yaml`,
  `templates/incident-reader.yaml` and `files/incident-schema.json`; and the kind test,
  `tests/e2e/run.sh`.

Update 2026-11-24: [0029. Report when kausa stopped seeing an incident's failure, and whether the change of a proposed action was observed, never that the incident was resolved, off by default](0029-incident-end-report.md) is proposed, not accepted.
It would amend points 1, 3, 4, 5, 6 and 9 of this ADR, its "Alternatives considered", its trust
statement, its Costs and its "Not verified", all with `incidents.reportEnd` on but point 9, which it
replaces. Point 1: the table of created Incidents keeps each entry in one slot of an array of at
most 500 from its create to its drop, so that a drop moves no other entry, and each entry gains 32
bytes (the term that created it, the action of the first Remediation that passed point 6's checks
and its time, a keyed hash of the container's name, the closing and outcome marks and a pointer to a
pending check of at most 80 bytes), within bar 3's 0.5 MiB, which is not raised; the gate, the
6-hour rule and the name do not change; and the `status` subresource stays declared and empty, so
"it is the place of the outcome check (point 9)" no longer holds, nor, in "Alternatives considered",
"The outcome check attaches to `Incident.status` instead (point 9)". Point 3: the owner cache gains
a second reader, 0029's read slot, which while a `rollout-undo` check is pending refreshes the
entries of the ReplicaSets its Pods name, and reads them past a fresh entry at acceptance, with the
same `get`, started at least 312.5 ms apart and never in the run loop; so an entry the fact slot
finds fresh may have been filled by it, and "At most one `get` per create attempted" counts the fact
slot's reads only. Costs: "Reads" gains those `get`s, at most 3.2 a second and 11,520 in any
half-open hour, apart from the creates' 500 in any 6 hours, the read slot's 4,608 bytes held while a
check is pending, and its one `get` in flight, whose body can be in transit while the fact slot's
answer is parsed, with at most one parse alive at a time (0029's Costs); "The table of created
Incidents" gains point 1's 32 bytes an entry. The trust statement: for a writer of Pods or
ReplicaSets in N, "Outside N, that wait is all it gets" no longer holds whole: its Pods and
ReplicaSets also feed those reads, in its own checks' turns, and a `get` it could make slow delays
the reads of other namespaces' checks, never the run loop; a writer of ReplicaSets can also make the
outcome check report `observed` by writing the revision annotation (0029's trust statement). What
the fact slot's wait costs does not change. Point 4: "The outcome check may raise the default" no
longer holds: the outcome check reads no Incident and does not raise the default of
`incidents.days`. Point 5: `incidents/status` keeps no verb, and "until the outcome check adds
`patch` there" no longer holds. Point 9 is replaced whole: the later ADR it names is 0029, which
reports the end of an incident by webhook and log and neither writes `Incident.status` nor moves
`incidents.days`; the slot stays reserved. The checks of point 6 do not change; its queue rule does:
when the queue is full the oldest `closed` or `outcome` message is dropped first, before any
`remediation`, and on the way out a `remediation` and the end messages leave in arrival order, after
every `detected` and `explained` message; the end messages have a bound and a counter of their own.
"Not verified" no longer needs the two `/status` tests before an outcome check. This takes effect
only when 0029 is accepted, which needs its bars measured and 0028 accepted; until then the text
above holds.
