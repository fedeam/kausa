# 0028. Publish each incident as an immutable Incident that only kausa writes, and report by webhook what kurita proposed for it, both off by default

- Status: proposed. Acceptance needs the bars of "Conditions of acceptance" measured, and
  [kurita's ADR 0008](https://github.com/fedeam/kurita/blob/main/docs/adr/0008-contract-with-kausa.md),
  the other half of this contract, accepted. Until then kausa writes no Incident and reads no
  Remediation, and the ADRs this one amends hold as written.
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
reports ([kurita](https://github.com/fedeam/kurita)). Its ADR 0008, the contract with kausa, is
written at the same time as this one and decides kurita's half; where this ADR restates that half,
it is so that kausa's half can be read alone, and kurita's ADR 0008 holds. Every figure of memory,
size, writes and latency below is **computed, not measured**, unless it says measured.

## Context

Status as of 2026-11-24: `main` at `588f3b5`, v0.6.3 released. 0027 is `proposed` on `main`; the
change that implements and accepts it is not merged that day, and the paths cited below
(`src/report.rs`, `src/fact.rs`) are that change's. Nothing below is implemented.

**What another controller would need, and kausa has no way to give.** kausa reports an incident
in its log, in a `Detected` Event and, with a webhook, in a `detected` message (0027). A
controller that proposes a remediation needs the same incident as typed data, from a source that
only kausa can write. None of today's outputs is one:

- **An Event is forgeable.** Whoever can create Events in a namespace, the built-in `edit` role
  included, can write one that looks like kausa's (0027's `Update 2026-11-24`). Events are kept one
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
  by a unit test (`src/report.rs:314-317`), against the bar of 0027's point 10 that a full queue
  adds at most 2 MiB.

**How the design was reached.** Two complete designs were written against the same brief and fixed
limits, and a review chose between them point by point, checking each one's claims against
kausa's source, kurita's ADRs and the Kubernetes documentation and source. The design below is the
result; "Alternatives considered" lists what was rejected and why. Two points the review left to
the maintainer are decided: the webhook's `v2` is amended before its first release so that a new
`type` fits in it (0027's `Update 2026-11-24`), and kurita compares the ReplicaSet's revision and
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
  120 bytes a Deployment in the design that proposed it). Neither it nor the ReplicaSet revision of
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
  compatibility text is amended instead (0027's `Update 2026-11-24`).
- **ConfigMaps, or annotations on workloads.** kausa reads no ConfigMap and writes no workload.

## Decision

Proposed on 2026-11-24. Both switches are off by default, and with the defaults kausa writes,
reads and sends what it does today.

### 1. The Incident

kausa publishes each incident as one object of a new resource, `incidents.kausa.fedeam.com`,
version `v1alpha1`, kind `Incident`:

- **Namespaced**, in the namespace of the workload it is about, named
  `kausa.<fingerprint>.<firstSeenUnix>` (the 16 hexadecimal characters of the fingerprint and the
  incident's first-seen time in Unix seconds), so one name per occurrence.
- **Immutable.** It is written by one `create` and never patched. A CEL rule `self == oldSelf` on
  its `spec` refuses any change of the `spec`, for every writer. The `status` subresource is
  declared and empty: it is the place of the outcome check (point 9).
- **Created in the fact slot of 0027's point 3**, after the incident's `Detected` Event (at once
  with `--no-events`), within the 10 seconds a write has there, and never before the incident's
  `incident` log line. A failed create is logged and counted (this ADR proposes
  `kausa_incident_write_errors_total`; the reference page fixes the name with the code) and not
  retried.
- **At most one create per fingerprint per `--dedup-hours`**, the window of 0027's point 5 (6 by
  default; with `0`, every occurrence). The gate reads kausa's own table of the Incidents it
  created in this process (point 6), never a list of the resource, and a fingerprint is marked
  only after a create the server accepted. The table keys it by namespace and fingerprint, as
  kurita keys its budget (its ADR 0008, point 8): the fingerprint already hashes the namespace
  (`src/fingerprint.rs`), but it is not cryptographic. kurita proposes at most once per
  fingerprint every 6 hours anyway, so more creates would be writes nobody uses.
- **No `ownerReference`** to the workload or to anything else, and nothing points to it.
- **The chart ships the resource as a template**, as it ships Explanations: turning incidents off
  or uninstalling deletes the resource and every Incident in the cluster.

### 2. Its schema is the second allowlist

The Incident's `spec`, structural, with no field kept unknown (a sketch; the reference page comes
with the code):

```yaml
spec:                       # CEL: self == oldSelf
  detection: D001..D014
  fingerprint: ^[0-9a-f]{16}$
  firstSeen: Time
  settledAt: Time
  object:                   # the incident's top-level owner
    apiVersion: at most 253, a group/version pattern
    kind: ^[A-Z][A-Za-z0-9]*$, at most 63
    name: Name
    uid: UID                # optional: an Event incident's owner may have none
  facts:                    # 0027's point 5 vocabulary
    reason: the compiled list of src/fact.rs   # optional
    exitCode: int32                            # optional
    container: DNS label, at most 63           # optional
    podsSeen: 1..100
    podsSeenIsMinimum: bool
    podsNow: 0..1000000
  sample:                   # optional
    pod: {name: Name, uid: UID}
    controller: {kind: ReplicaSet | Job | StatefulSet | DaemonSet | Other,
                 uid: UID,
                 revision: int64 1..2^53}      # CEL: only with kind ReplicaSet
  deployment: {generation: int64 >= 1, revision: int64 >= 1}   # CEL: D014 only
  regarding:                # an Event incident about something other than a Pod
    kind: Job | ReplicaSet | StatefulSet | DaemonSet | CronJob | PersistentVolumeClaim
          | HorizontalPodAutoscaler
    name: Name
    uid: UID                # optional
  missing: at most 2 of owner-lookup-failed | revision-lookup-failed, a set
```

`Time` is the pattern `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$` with at most 20 characters; `UID`
a UID pattern of 36 characters; `Name` a DNS-1123 subdomain of at most 253. Every string is an
enum, or a pattern with a maximum length; a test walks the schema to check it, and another checks
that the `reason` enum equals the list compiled in `src/fact.rs`. At the longest names the `spec`
is about 1.5 KB (an estimate; bar 6 measures the stored object).

The schema is kausa's second allowlist, beside invariant 12's fact statement: an Incident holds
only the fact statement's facts and the typed values listed here, never a message, a note, a log
line, an image, a label, a node or any text a model wrote. Changing what it may hold needs an ADR,
and invariant 12 says so (point 8). Within `v1alpha1` changes are additive only.

### 3. The data kausa keeps for it

Each entry of the fact queue gains one optional pointer, filled only with incidents on:

- **The sample Pod's UID.**
- **The sample Pod's controller:** its kind, its UID from the Pod's own `ownerReference`, and, for a
  ReplicaSet, the revision in its `deployment.kubernetes.io/revision` annotation, read by the
  existing cached metadata `get` of the owner lookup. The controller's name is not kept: kurita
  finds the ReplicaSet by UID with its own `list`. **The owner cache keys an entry by hop,
  namespace and name, not by UID, and keeps it 600 seconds**, so the revision in
  `sample.controller` can be older than the live one after a new rollout, and newer after the
  ReplicaSet was deleted and recreated under the same name. kurita compares for equality (its ADR
  0008, point 4), so either difference stops an undo proposal; a recreation also changes the UID,
  which it compares.
- **`regarding`**: the kind, the name and, when the Event has one, the UID of the object an Event
  incident is about, when that object is not a Pod.
- **`missing`**: which of the two lookups failed, `owner-lookup-failed` or
  `revision-lookup-failed` (the annotation absent or not a whole number).
- **For `D014`**, the Deployment's `metadata.generation` and revision at detection, from the record
  kausa already holds while the rule matches.

UIDs are kept as 16 binary bytes. Each owner-cache entry gains the revision, 8 bytes. No table of
Deployments is added, and no Pod beyond the sample.

### 4. Retention

| Limit | Set by | Default |
|---|---|---|
| Age, by `metadata.creationTimestamp` | `incidents.days`, 1 to 30 | 1 |
| Incidents per kausa | constant, the same as 0014's `MAX_OBJECTS` | 500 |

- **0014's sweep, applied to Incidents**: at start and hourly, for at most 30 seconds, in pages of
  2 (0014's `Update 2026-10-28`), dropping the `metadata` it does not read before it reduces each
  object. It deletes, with UID and `resourceVersion` preconditions, the Incidents older than
  `incidents.days` and, through a bounded heap, the oldest beyond 500. Which slot runs it is
  decided with the code, under invariant 6: a fact never waits for it longer than for a write.
- **"Oldest" is by `metadata.creationTimestamp`**, which the API server sets and no writer can,
  never by `firstSeen`, which any writer of Incidents sets.
- **One day covers kurita's window**: kurita decides an Incident only while it is younger than 6
  hours by `creationTimestamp` (its ADR 0008, point 3). The outcome check may raise the default.
- **The cap is shared across namespaces**, as 0014 says of its own: it protects the datastore, not
  fairness. A tenant whose workloads fail under many fingerprints, or anyone who can create
  Incidents, can fill it, and then each sweep deletes the oldest Incidents of every namespace,
  younger than 6 hours included. What that achieves: kurita proposes nothing for an Incident deleted
  before kurita decided it, which happens only while kurita is down or behind; kausa still writes
  its Events and posts its `detected` messages, a Remediation already made stays (no
  `ownerReference`), and kausa still reports it, since it reports from its own memory. A
  ResourceQuota on `count/incidents.kausa.fedeam.com` in a namespace bounds what that namespace can
  hold.

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

- **The lookup is in kausa's own memory.** kausa keeps a table of the Incidents it created in this
  process: at most 500 entries, the cap of point 4, the oldest dropped first. For a Remediation in
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
- **`none` is never sent.** It means kurita found nothing to propose; `needs-human` and `inspect`
  are sent, since each tells a person something.
- **At most one message per Incident.** The table marks an Incident once its message is queued.
- **A record that fails a check is dropped**, logged as `remediation ignored` through `log.rs` with
  its reason and its namespace and name, each checked against its pattern first, and counted in
  `kausa_remediations_ignored_total{reason}` (the six reasons above). It is never retried, and no
  part of it is sent.
- **The bound is shared:** `remediation` messages count against `--notify-max-per-hour` with the
  `detected` ones, and over it they are `capped` as those are (0027's point 5).

**The message**, `type: remediation` within `v2` (0027's `Update 2026-11-24`: a value of `type`
may be added within `v2`, and a receiver must ignore a message whose `type` it does not know):

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
  reused, except a Remediation's action id and `proposedAt`, which, after the checks of 0028,
  are sent as a `remediation`
  message's action and time, and the namespace and name of a dropped Remediation, logged after
  their patterns are checked."
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
  ship a new id without a kausa release.
- **After a restart or a failover** the table is empty, and it is never seeded from a list of
  Incidents (invariant 5): Remediations of earlier Incidents are not reported (`incident-unknown`),
  and kausa may create a second Incident for the same fingerprint, under a new first-seen time;
  kurita's budget per fingerprint absorbs it (`needs-human{cooldown}`). With `--leader-lease`,
  only the holder creates, sweeps and reports.

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

- **A writer of Incidents can:** make kurita check the live objects again, in N only; spend
  kurita's proposal budget; suppress an undo proposal, or let one through, by forging the
  revisions of point 3; fill the cap of point 4; and make kausa send nothing about it, since kausa
  reports only from what it holds in memory.
- **A writer of Remediations in N can:** make kausa send one false "kurita proposed X", where X is
  an action id allowed for a real incident kausa detected in N, at most once per Incident; and
  take kurita's claim first, so that kurita's create answers `AlreadyExists` and kurita proposes
  nothing for that Incident, unable to tell that the record is not its own. Outside N, it can
  spend kurita's cluster budget, silence kurita by filling kurita's cap, or, by deleting records,
  let kurita propose more than its budget allows in an hour (kurita's ADR 0008, its trust
  statement); none of that makes kausa send anything.

**Already true before this contract, and out of its scope:** editors of kausa's namespace can read
the webhook's Secret (`charts/kausa/values.yaml:96-99`) and post anything to the receiver
directly; and anyone who can create Events can make kausa detect a forged Event incident (0027).

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
  the design review (dominated by a `regarding` name of up to 253 bytes; the UIDs kept as 16 binary
  bytes), not derived field by field here; bar 2 measures it. Then (1,336 + 320) × 1024 =
  1,695,744 bytes = 1.62 MiB computed, and about 1.82 to 1.88 MiB resident, at the ratio of
  measured to computed in 0027's prototype: in kind, the resident memory grew by 1.74 MiB for the
  queue alone; the 1.80 MiB is for the queue and the table of notified fingerprints together, both
  in kind (the control group's anonymous memory) and on the Raspberry Pis (`VmRSS`, 1,844 to 1,848
  KiB); over 1,583 × 1024 = 1,620,992 bytes = 1.55 MiB computed for the queue, a ratio of 1.13 to
  1.16 (1.74 / 1.55 and 1.80 / 1.55). The upper ratio counts the table (40 KiB computed) as if it
  were the queue's, so it errs high, and 1.88 MiB overstates. On the Raspberry Pis the prototype
  did not measure the queue alone ([The fact statement prototype,
  measured](../research/2026-11-20-fact-statement-prototype.md)).
- **The table of created Incidents:** about 48 bytes an entry without the report and at most about
  750 bytes with it, estimates carried from the design review, not derived here; bar 3 measures
  them. Then 500 × 48 = 24,000 bytes, about 23 KiB, and 500 × 750 = 375,000 bytes, 0.36 MiB.
- **The owner cache:** +8 bytes an entry, the revision.
- **Writes per incident:** kausa +1 create and, later, +1 delete; kurita +1 create and +1 delete,
  besides its own Event. **Writes per change of an object:** 0; neither resource is patched.
- **Stored objects:** an Incident at most about 2.5 KB (a `spec` of about 1.5 KB at the longest
  names, plus metadata) and a Remediation at most about 7 KB, estimates carried from the design
  review; bar 6 measures them with `managedFields`. At 500 of each: 500 × 2.5 KB = 1.25 MB of
  Incidents and 500 × 7 KB = 3.5 MB of Remediations.
- **Watches:** kausa 1 on `remediations`, across the cluster or 1 per listed namespace, only with
  the report on; kurita 1 on `incidents`, or 1 per listed namespace. kurita's share of the events
  on kausa's watch is one create and one delete per Incident kausa creates, for every outcome, so
  it follows kausa's rate of Incidents, which kausa's create gate (one per fingerprint per
  `--dedup-hours`) and its cap of 500 bound. kurita never deletes a Remediation
  younger than 6 hours and creates none while 500 exist, so kausa's watch sees at most 500 creates
  by kurita in any 6 hours, plus the deletes of older records. Writes to `remediations` by anyone
  else are not bounded by this ADR or by kurita's.
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
   kausa or kurita; the drain time recorded against the 7.9 to 21.5 seconds of Events alone.
5. A sweep of 500 objects causes no relist.
6. The stored sizes of both resources, `managedFields` included, on k3s with its default
   datastore, recorded against the estimates of 2.5 KB and 7 KB above. Observational: it does not
   condition acceptance by itself, and a size over the estimate amends the costs.
7. Both CustomResourceDefinitions are accepted within the CEL cost budget on the oldest supported
   k3s.

kurita's ADR 0008 adds two bars of its own (its 8 and 9), on its relist and its cap; the two ADRs
are accepted together. If bar 2 fails after the trims of point 3, the maintainer chooses between
dropping `regarding.name`, so that kurita gives `needs-human{unconfirmable}` again for `D008` with
a controller and for `D011`, and changing 0027's point 10; both ADRs are amended accordingly.

### Not verified

- **Whether the metadata `get` of a ReplicaSet returns its annotations,** where the revision is.
  Test: read a ReplicaSet with a metadata-only `get` in kind and check the
  `deployment.kubernetes.io/revision` annotation.
- **The page size of kube-rs's watcher on its initial list.** Test: read its source, or count the
  list requests against 500 Remediations in kind.
- **That a `/status` write cannot change `metadata` or `spec`, and that root CEL rules run on
  `/status` writes:** checked in the source only. Test both in kind before the outcome check
  relies on them.
- **The CEL cost of the per-detection rules.** Test: install both CustomResourceDefinitions on the
  oldest supported k3s (bar 7).
- **Nothing above is measured.** The Event latency is computed from measured totals.

## Consequences

- **Another controller can read kausa's incidents as typed data**, from a resource only kausa's
  service account and the principals of the trust statement can write, without kausa reading,
  writing or executing anything more on workloads.
- **More writes, only when turned on:** one create and one delete per incident, and, with the
  report, one watch and one message per proposal. With the defaults nothing changes.
- **kausa reads another project's resource** for the first time, `remediations`, and writes none.
  Every field of it that leaves is typed and checked; a writer of Remediations gains one false
  notice per Incident at most.
- **A restart loses notices, never invents them:** Remediations of Incidents created before it are
  not reported.
- **The shared cap is not fair:** one tenant's volume, or a forger, can make the sweep delete other
  namespaces' Incidents before kurita decides them; kausa's own Events and messages are unaffected.
- **Pages that change with the implementation**, not with this ADR: a new reference page for the
  Incident resource, `docs/reference/incident-resource.md`;
  [Permissions](../reference/permissions.md), [Helm chart](../reference/helm-chart.md)
  (`incidents.enabled`, `incidents.days`, `incidents.installCRD`,
  `incidents.readers.aggregate`, `incidents.reportRemediations`), [Data sent in
  notifications](../reference/data-sent-in-notifications.md), [Command line](../reference/command-line.md)
  (the flags, log lines and metrics), [Alerts](../reference/alerts.md),
  [ARCHITECTURE.md](../../ARCHITECTURE.md) (invariants 2, 5, 11 and 12), the design principle of
  [README.md](../../README.md), the [Security policy](../../SECURITY.md),
  [Architecture](../explanation/architecture.md) and [CHANGELOG.md](../../CHANGELOG.md); the
  chart's `values.yaml`, `templates/_helpers.tpl`, `templates/incident-crd.yaml`,
  `templates/incident-reader.yaml` and `files/incident-schema.json`; and the kind test,
  `tests/e2e/run.sh`.
