# Incident resource

The custom resource in which kausa publishes each settled incident's facts with `--incidents`: its
CustomResourceDefinition, each field and its bounds, the objects kausa creates and deletes, what an
Incident never holds, the refusals, the counters and who can read it.

Status as of 2026-11-25: in `main` for the release after v0.6.3, not released; off by default
(`--incidents`, the chart's `incidents.enabled`). The decision, [0028. Publish each incident as an
immutable Incident that only kausa writes, and report by webhook what kurita proposed for it, both
off by default](../adr/0028-incident-resource-and-remediation-report.md), is `proposed`; this page describes
its points 1 to 5 as the code implements them, and nothing of its remediation report (point 6),
which is not implemented: kausa reads no `remediations` and has no flag or chart value for them.
The kind test (`tests/e2e/run.sh`) has cases that install the resource with `incidents.enabled=true`
in both `rbac.scope`s and read the Incidents back; as of 2026-11-25 they have never run.

## The CustomResourceDefinition

| Field | Value |
|---|---|
| Name | `incidents.kausa.fedeam.com` |
| Group, version | `kausa.fedeam.com`, `v1alpha1` (served and storage, the only one; conversion `None`) |
| Kind, list kind | `Incident`, `IncidentList` |
| Plural, singular | `incidents`, `incident`; no short names and no categories, so it stays out of `kubectl get all` |
| Scope | `Namespaced`: each object is in the namespace of the workload it is about, next to its Events |
| Subresources | `status`, declared and empty: kausa writes no status, and the schema keeps no field in it |

The chart installs it from `templates/incident-crd.yaml` only while the release creates Incidents
(`incidents.enabled` and not `dryRun`) with `incidents.installCRD` ([Helm chart](helm-chart.md));
kausa never creates, changes or reads a CustomResourceDefinition. Its `openAPIV3Schema` is
`charts/kausa/files/incident-schema.json`, which the chart includes with `.Files.Get` and
`fromJson`, and which the unit tests of `src/incident_resource.rs` read, to check that its
patterns, limits and enums equal kausa's constants and that every string is an enum or a pattern
with a maximum length. The schema is structural, so the API server prunes a field it does not list,
and every field has a description, so `kubectl explain incident.spec.facts` documents it. It has no
printer columns: `kubectl get incidents` shows the name and the age.

## Fields and limits

`Time` is `^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$`, at most 20 characters (UTC to
the second); `UID` a UID pattern of 36 characters; `Name` a DNS-1123 subdomain of at most 253.

| Field | Type and limit | Content |
|---|---|---|
| `spec` | object, required, immutable | The incident as kausa read it when it settled. CEL rule `self == oldSelf`, message `spec is immutable`, for every writer, a cluster administrator included. Required, because a transition rule is not evaluated when there is no old value |
| `spec.detection` | `D001` to `D014`, required | The detection id ([Detection and redaction rules](rules.md)) |
| `spec.fingerprint` | `^[0-9a-f]{16}$`, required | The incident's fingerprint |
| `spec.firstSeen` | `Time`, required | When kausa first saw the failure; the name holds it in Unix seconds |
| `spec.settledAt` | `Time`, required | When the incident settled and kausa reported it |
| `spec.object.apiVersion` | a group and a version, or a version alone, at most 253, required | The top-level owner's `apiVersion`, the `regarding` of its `Detected` Event |
| `spec.object.kind` | `^[A-Z][A-Za-z0-9]*$`, at most 63, required | Its kind |
| `spec.object.name` | `Name`, required | Its name |
| `spec.object.uid` | `UID`, optional | Its UID, when kausa holds one: an Event incident's owner may have none |
| `spec.facts.reason` | an enum of 32, optional | The sample's reason, when it is on the list of a detection; the enum equals the list compiled in `src/fact.rs` |
| `spec.facts.exitCode` | `int32`, optional | The sample container's exit code, when it has one |
| `spec.facts.container` | DNS-1123 label, at most 63, optional | The sample container's name |
| `spec.facts.podsSeen` | 0 to 100, required | Pods kausa saw failing, 100 for 100 or more; 0 exactly for `D008` and `D011` to `D014`, which are about an object that is not a Pod |
| `spec.facts.podsSeenIsMinimum` | boolean, required | `true` exactly when `podsSeen` is 100: a lower bound |
| `spec.facts.podsNow` | 0 to 1,000,000, required | Pods of the owner kausa held in the namespace when the incident settled |
| `spec.sample.pod.name`, `spec.sample.pod.uid` | `Name`, `UID`, required in `sample` | The sample Pod |
| `spec.sample.controller.kind` | `ReplicaSet`, `Job`, `StatefulSet`, `DaemonSet` or `Other`, required in `controller` | The sample Pod's controller `ownerReference`; `Other` for any other kind. `controller` is absent for a Pod with none |
| `spec.sample.controller.uid` | `UID`, required in `controller` | The controller's UID, from the Pod's `ownerReference` |
| `spec.sample.controller.revision` | `int64`, 1 to 2^53, optional | A ReplicaSet's `deployment.kubernetes.io/revision`, read just before the create; absent exactly when `missing` names `revision-lookup-failed` |
| `spec.deployment.generation`, `spec.deployment.revision` | `int64` of at least 1; 1 to 2^53 | For `D014`, the Deployment's `metadata.generation` and revision when the rule matched |
| `spec.regarding.kind` | `Job`, `ReplicaSet`, `StatefulSet`, `DaemonSet`, `CronJob`, `PersistentVolumeClaim` or `HorizontalPodAutoscaler`, required in `regarding` | For an Event incident about an object that is not a Pod, that object's kind |
| `spec.regarding.name`, `spec.regarding.uid` | `Name`, required; `UID`, optional | Its name, and its UID when the Event gives one |
| `spec.missing` | a set of at most 2 of `owner-lookup-failed` and `revision-lookup-failed`, required | Which of kausa's reads failed; empty when none did |
| `status` | object, empty | kausa writes no status |

The CEL rules on `spec`, besides immutability, each with its message:

- `sample is set exactly when podsSeen is above 0`;
- `podsSeen is 0 exactly for D008 and D011 to D014`;
- `deployment is set exactly for D014`;
- `missing names revision-lookup-failed exactly when a ReplicaSet controller has no revision`;
- `sample and regarding are never both set`;
- `missing names owner-lookup-failed only for an incident about a Pod, D008 or D011`;
- `a D008 or D011 with owner-lookup-failed names in object the object of regarding, a Job of
  batch/v1 or a ReplicaSet of apps/v1`.

And one on `facts` (`podsSeenIsMinimum is true exactly when podsSeen is 100`) and one on
`sample.controller` (`only a ReplicaSet has a revision`). `facts` holds the values of the fact
statement but its revision, which only `D014` has and which `deployment.revision` holds.

## Objects

- **One object per occurrence**, named `kausa.<fingerprint>.<firstSeenUnix>`: the 16 hexadecimal
  digits of the fingerprint and the first-seen time in Unix seconds, 1 to 10 digits
  (`^kausa\.[0-9a-f]{16}\.[0-9]{1,10}$`). A `firstSeen` before 1970 or at or after
  2286-11-20T17:46:40Z gets no Incident: a refusal with the reason `schema`.
- **Immutable:** written by one `create` and never patched.
- **No label, no annotation and no `ownerReferences`**, and nothing points to it.
- **In the namespace of the workload** it is about.

## What kausa creates

In the fact slot of [0027](../adr/0027-fact-statement-for-the-detections.md), after the incident's
`incident` log line, its `Detected` Event (at once with `--no-events`) and its `detected` message,
with or without `--endpoint`, `--no-events` or a webhook. In this order:

1. **The gate.** At most one create per fingerprint and namespace per `--dedup-hours` (6 by
   default; with `0`, every occurrence). The gate reads kausa's own table of the Incidents it
   created in this process, never a list of the resource. A duplicate inside the window stops here,
   silently.
2. **The first sweep.** Until a sweep of this process has ended (completed, failed, or stopped at
   its bound), the create is refused with the reason `starting`. A start sweep cut short by the
   loss of the term is followed by the next term's.
3. **Room in the table.** The table holds at most 500 entries (`CAPACITY`), each kept for at least
   6 hours (`KEEP`), the window in which kurita decides an Incident; older entries are dropped,
   oldest first, when room is needed. While it holds 500 entries younger than 6 hours, the create is
   refused with the reason `table-full`. So one process creates at most 500 Incidents in any 6
   hours, in every namespace together.
4. **The schema, before the read.** kausa never sends a create the schema would refuse: each value
   read from the cluster (`object`, `sample.controller.uid`, `regarding`, `deployment`,
   `firstSeen` and the name it sets, `podsNow`) is checked against its field; one that does not fit
   is a refusal with the reason `schema`, and no request is made for it.
5. **The revision.** For a sample Pod run by a ReplicaSet, one metadata `get` of that ReplicaSet,
   by the name of the Pod's `ownerReference`, through the owner lookup kausa already does, to read
   its `deployment.kubernetes.io/revision`. An answer with another UID than the `ownerReference`'s,
   or an annotation absent or not a whole number from 1 to 2^53, leaves `revision` out and adds
   `revision-lookup-failed` to `missing`. The ReplicaSet's name is kept only until this read, and
   is never written.
6. **The create**, within 10 seconds (`WRITE_BOUND`), reading at
   most 64 KiB of the answer (`CREATE_ANSWER_BYTES`). Only the UID the server gave it is kept, in
   the table, with the time the answer arrived on kausa's monotonic clock; the fingerprint is then
   marked for the gate.

A refusal sends nothing, and a failed create (no answer within 10 seconds, an answer over 64 KiB or
with no UID, `409 AlreadyExists`, or any error) is logged, counted and not tried again; neither
marks the fingerprint, so a later occurrence tries again. A create the server stored but kausa got
no valid answer for leaves no entry, so it does not count toward the 500. Refusals and failures do
not affect the incident's Events, its `incident` line or its `detected` message. With
`--leader-lease`, the read and the create are made only while this replica holds the Lease. A
restart or a takeover empties the table: the gate does not survive it, and a new process can create
an Incident for an incident the last one already published.

## Retention

| Limit | Set by | Default |
|---|---|---|
| Age, by `metadata.creationTimestamp` | `--incident-days`, chart value `incidents.days`, 1 to 30 | 1 |
| Incidents across the namespaces one kausa covers | constant `MAX_OBJECTS`, the same as the Explanations' | 500 |

The sweep, the Explanations' sweep applied to Incidents, runs in a slot of its own, at the start of
each term and then once an hour, for at most 30 seconds (`SWEEP_TIME`), so a fact never waits for it
longer than for a write. It lists every Incident, with no label selector, in pages of 2
(`SWEEP_PAGE`), across the cluster or in each `--namespace` in order, keeps only five fields of each
object's `metadata` (name, namespace, UID, `resourceVersion` and `creationTimestamp`) before it
reduces it, deletes those created more than the days ago as each page arrives, and keeps the others
in a bounded heap: beyond 500, the oldest is deleted for the cap. "Oldest" is by
`creationTimestamp`, which the API server sets, never by `firstSeen`, which any writer of Incidents
sets. Each delete carries the listed UID and `resourceVersion` as preconditions, so an object
replaced or changed since the list is kept until the next sweep; a delete answered `404` or `409` is
neither an error nor a deletion. A failed list ends its namespace's part of the sweep, and the next
namespace goes on; a call that does not end within 10 seconds ends the sweep.

The cap is the sweep's target, shared across namespaces: it protects the datastore, not fairness.
After a sweep that completes, the count kausa causes stays at most 500 + 500 = 1,000 until the next
one. In a crash loop, each start whose sweep does not complete can leave up to 500 more. Incidents
written by anyone else are not bounded by kausa; a ResourceQuota on
`count/incidents.kausa.fedeam.com` bounds what a namespace can hold. Nothing prunes Incidents while
kausa is not running or runs without `--incidents`; in the chart with `incidents.installCRD`, the
CustomResourceDefinition, and with it every Incident in the cluster, is deleted when the release
stops creating Incidents or is uninstalled.

## What an Incident never holds

- **No text a tenant or a model wrote:** never a message, an Event note, a log line, an image, a
  label, a node, or any text a model wrote. The schema is kausa's second allowlist, beside the
  [fact statement](rules.md#fact-statements): an Incident holds only the fact statement's facts and
  the typed values above, and changing what it may hold needs an ADR (invariant 12 of
  [ARCHITECTURE.md](../../ARCHITECTURE.md)).
- **No metadata of its own** but its name and namespace: no `ownerReferences`, labels or
  annotations, and an empty `status`. The ReplicaSet's name read for the revision is never written.

Nothing new leaves the cluster: Incidents go to the API server only, and nothing read back from one
is logged, sent or reused. Of each Incident it lists, the sweep uses the namespace and name only
to build the path of its delete and echoes the UID and `resourceVersion` only as that delete's
preconditions, and uses `creationTimestamp` only to rank; the list's `continue` token is only sent
back to ask for the next page.

## Refusals, log lines and counters

| Reason | When |
|---|---|
| `table-full` | The table holds 500 entries younger than 6 hours |
| `schema` | A value read from the cluster does not fit its field, or the name cannot be built |
| `starting` | The create arrived before the first sweep of this process ended |

| Line | Level | Fields |
|---|---|---|
| `incident refused` | warn | `reason`, `namespace` (when it is a DNS label), `fingerprint`, `field` (for `schema`: the path of the field that did not fit, such as `object.name`, never its value). At most once an hour per reason, across namespaces; the counter counts every refusal |
| `incident write failed` | warn | `fingerprint`, `namespace`, `error`: `no answer within 10 seconds`, `the answer is over 65536 bytes`, `the answer carries no UID`, `the answer could not be read: ...`, `the term ended before the read` or `before the create` (with `--leader-lease`), `the table is full` or `the entry was refused`, or, when the API server refused the create, its code and reason only, since its message can quote the object; kube-rs's text for any other failed request ([Command line and log format](command-line.md#log-lines)) |
| `incident sweep failed` | warn | `namespace` (the listed namespace whose list or delete failed, or `null` in cluster scope), `error` (`the CustomResourceDefinition incidents.kausa.fedeam.com is not installed` when the list answered `404`; otherwise the code and reason only) |
| `incidents swept` | info | `deleted_by_age`, `deleted_by_cap`: what one sweep deleted, only when it deleted something |

| Counter | Labels | Meaning |
|---|---|---|
| `kausa_incidents_refused_total` | `reason` (`table-full`, `schema`, `starting`) | Incident objects not created, by reason |
| `kausa_incident_write_errors_total` | | Incident objects that could not be created |
| `kausa_incidents_deleted_total` | `reason` (`age`, `cap`) | Incident objects deleted by the sweep, by reason |
| `kausa_incident_sweep_errors_total` | | Failed list or delete calls of the incident sweep (a delete answered `404` or `409` is not one) |

The four exist only with `--incidents`, and then every series shows 0 from the start; without it
the exposition has no line of them ([Command line and log format](command-line.md#metrics)).

## Who can read it

kausa itself has `create`, `list` and `delete` on `incidents`, only with incidents on, and never
`get`, `watch`, `patch`, `update`, `deletecollection` or `incidents/status` ([Permissions](permissions.md)).

With `incidents.installCRD` and `incidents.readers.aggregate` (both true by default), the chart
also installs a ClusterRole `<full name>-incident-reader` with `get`, `list` and `watch` on
`incidents`, labelled `rbac.authorization.k8s.io/aggregate-to-view: "true"`. Kubernetes aggregates
it into the built-in `view` role, and through `view` into `edit` and `admin`, so whoever has one of
them in a namespace reads its Incidents as they read its Events. No write is aggregated: only kausa
and cluster administrators write Incidents. The reader role is bound to nobody and grants kausa
nothing.

```sh
kubectl get incidents -n shop
kubectl get incident -n shop kausa.5d0c2b7e9a1f4c38.1764064800 -o yaml
```

## Schema changes

Within `v1alpha1`, changes are additive only: new optional fields, each of which needs an ADR,
since the schema is an allowlist. A breaking change is a new version without a conversion webhook,
or a release that lets Helm replace the CustomResourceDefinition and delete the old objects, under a
breaking-change heading in the [Changelog](../../CHANGELOG.md). Every change to
`charts/kausa/files/incident-schema.json` is a line in the Changelog.
