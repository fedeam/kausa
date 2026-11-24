# 2026-11-24 — The fact statement

## Request

Measure the prototype of [0027. Report each settled incident as a fact statement in its own
`Detected` Event and webhook message, with or without a model, and mark the model's text as a
model's](../adr/0027-fact-statement-for-the-detections.md) on a Raspberry Pi 4, accept it if its
three bars hold, and implement its points 1 to 8 from the prototype, with tests first.

## What was done

- **The measurement on a Raspberry Pi 4**, in a k3s cluster of three with about 100 Pods, v0.6.2
  and the prototype side by side on one node: the three bars of point 10 hold. The first was read
  in the kubelet's `rssBytes`; a check on the same nodes, after the runs, found that figure
  follows the anonymous memory of the control group to within 20,480 bytes
  ([The fact statement prototype,
  measured](../research/2026-11-20-fact-statement-prototype.md#on-a-raspberry-pi-4)). A first
  version of the Event bar was discarded: deleting Events while a watch on Events is open makes
  k3s with kine end that watch with an error, whatever writes the Events.
- **0027 accepted** on 2026-11-24, under a plan FedeAM approved that day that delegates the
  acceptance of ADRs within its limits; `Update` paragraphs in 0015 and 0008, and invariants 6 and
  12 of [ARCHITECTURE.md](../../ARCHITECTURE.md).
- **The implementation**, ported from the prototype rather than merged:
  - `src/fact.rs`: the statement, its byte set, the list of reasons, the DNS-label check of the
    container, the longest statement (286 bytes) pinned by a test, and a fuzz target `fact`.
  - `src/report.rs`: the fact queue of 1024 that drops its oldest, and the fact slot that writes
    the `Detected` Event and queues the `detected` message; it never waits for the model.
  - `src/events.rs`: the `Detected` Event, and the mark `Model's explanation, not verified:` on
    the `Explained` note.
  - `src/notification.rs` and `src/notify.rs`: the webhook schema `v2` (`detected`, `explained`,
    `capped`, `facts`), the deduplication of `detected` messages per `--dedup-hours`, and
    `--notify-max-per-hour`. `--notify-url-file` no longer needs `--endpoint`.
  - The metrics and log lines of point 8; the chart grants Event writes without an endpoint, and
    has `notifications.maxPerHour`.
  - The kind test checks the `Detected` Events with and without a model, in both scopes, and the
    upgrade from v0.6.3.
- **`D013` renamed** to "autoscaler cannot read metrics or target scale", as point 1 of 0027
  asks, in its own commit; the id stays `D013`.
- **A review of the branch** by a documentation reader and a security reader, closed in new
  commits:
  - a message dropped by the queue of 16 or by the fact queue now owes the `capped` message, as
    one over the limit per hour did, with a unit test first;
  - the Slack and Telegram texts keep an owner's kind and name on one line. In kind, with
    Kubernetes v1.36.4, the API server accepted a Pod whose ownerReference had a line break in
    its kind and name, and an Event whose `regarding` had one;
  - the [Security policy](../../SECURITY.md) says a `Detected` Event's name can be taken first;
  - texts made false by the fact statement (the chart's comments, the README, `AGENTS.md`), the
    `D013` rename in the notes for receivers of `v1`, a quoted error from the measured cluster,
    the queue figures of the prototype against the code's, and the `rssBytes` caveat;
  - 0027's body back to the proposal's text, with what the acceptance adds in its `Update`.
- **The kind test** checks, in both scopes, that each `Explained` Event is of the incident of a
  `Detected` Event, and, with a spent budget, a `Detected` and an `ExplanationSkipped` Event of
  one incident. A check of their order was tried and dropped: in kind an `Explained` Event was
  written before its `Detected` Event when several incidents settled at once, as point 3 of 0027
  allows; the `Update` of 0027 and the pages on Events and notifications say either can come
  first.
- **The kind test's phases without a model** count the receiver's lines with no kausa Pod left:
  in cluster scope on GitHub, the Pod restarted before them was still posting, its tables per
  process, and one workload got two `detected` bodies in the count.

## Decisions

- The fact queue keeps the typed facts and renders the statement from them when it writes, so
  the webhook's `facts` object is built from them, not parsed back from the text.
- The `Facts:` line of a text notification is left out when it would be empty, and the notice of
  a withheld answer comes after the mark.
- `D013`'s new name has no apostrophe, since a statement's bytes allow none.
- A `detected` message never pushes an `explained` one out of the notification queue: the
  explanation cost a model call and is sent once, and the facts stay in the Event and the log
  ([0027](../adr/0027-fact-statement-for-the-detections.md), its `Update`).
- An owner's kind and name are made one line where the text is rendered, not where they enter:
  that covers ownerReferences and `regarding` alike, and the `json` body, the Events and the log
  keep the value, escaped as JSON.
- Within notification `v2` a value of `type` may be added, and receivers ignore one they do not
  know.

## Follow-ups

- Which terminated reasons other than `Error` the restart-count path of `D001` meets, read in the
  kubelet's and containerd's code: still a hypothesis (point 10).
- The fuzz target `fact` runs in ClusterFuzzLite, not on the maintainer's machine.
- The cost of the webhook path is in no measured figure.
- The kind test checks that kausa sends no `type` outside the three of `v2`; a new type changes
  that check with it.

## The Incident resource

### Request

Write the ADR that records kausa's half of the contract with kurita, a controller that proposes
remediations for kausa's incidents: an `Incident` resource that only kausa writes, and the report
by webhook of what kurita proposed. Docs only; kurita's ADR 0008, its other half, was already on
kurita's `main` as `proposed`.

### What was done

- [0028. Publish each incident as an immutable Incident that only kausa writes, and report by
  webhook what kurita proposed for it, both off by default](../adr/0028-incident-resource-and-remediation-report.md),
  `proposed`. It records the design chosen by a review of two complete proposals, aligned field by
  field with kurita's ADR 0008: the Incident's schema, the data kausa keeps for it, retention,
  permissions, the `remediation` message of the webhook's `v2`, the trust statement, the costs
  computed and the bars that must be measured before both ADRs are accepted.
- `Update 2026-11-24` paragraphs at the end of 0001, 0009, 0015 and 0027, each saying what 0028
  would change there and that it takes effect only when 0028 is accepted.
- A security and a docs review of the proposal changed it the same day: `remediation` messages got
  an hourly bound of their own, so they never cost a `detected` or an `explained` message; the
  table of created Incidents never drops an entry younger than 6 hours, and kausa creates no
  Incident while 500 such entries exist; the trust statement gained whoever can create Events;
  the Incident's `spec` is required, so that its immutability rule always runs. A second
  security round made `remediation` messages leave the queue only when no other message waits,
  gave them a counter of their own, and stated how long one forged Event every 43.2 seconds keeps
  new Incidents stopped.
- While 0029 was reviewed, its author found that 0028 never said how the tick that emits an Incident
  reads the sample's ReplicaSet revision: the tick makes no request, a Pod's owner is resolved once,
  and its owner-cache entry is gone after 10 minutes, so for most long-running workloads the
  revision would have been absent and kurita would almost never have proposed an undo. 0028 now
  reads it in the fact slot, before the create.
- A seventh review of 0028 made that read happen only for a create kausa will send, after the gate
  and the table's room, bounded it per create attempted, since a failed create is not retried and
  reads again at the next occurrence, and stated what a writer of Pods gets outside its namespace: a
  wait of the fact slot of at most 5 seconds per create. The Incident carries the revision as of
  that read, not of detection, and kurita's change check counts from the read. `owner-lookup-failed`
  also covers the owner lookup of an Event incident about a Job or a ReplicaSet, and 0027's `Update`
  says the slot can hold the read. An eighth review found that a writer of ReplicaSets or Pods could
  still make a create fail, with a revision of 0 or an owner's UID that does not parse; 0028 now
  says kausa never sends a create the schema would refuse.
- Nothing is implemented, and nothing in it is measured.

### Decisions

- Proposed only: [0028](../adr/0028-incident-resource-and-remediation-report.md). It is accepted
  together with kurita's ADR 0008, once the bars of its "Conditions of acceptance" are measured on
  the Raspberry Pis and in kind.
- In [0028](../adr/0028-incident-resource-and-remediation-report.md), the sample's ReplicaSet
  revision is read in the fact slot, after the create gate and before the create, through the cached
  owner lookup: at most 600 seconds old, at most one `get` per create attempted, and a failed read
  is `revision-lookup-failed`. Keeping the revision with each Pod's resolved owner was rejected,
  since it is as old as the Pod; a third value of `missing` alone, since it would leave undo
  proposals rare. The design is the ADR author's, added after the design review.
- In [0028](../adr/0028-incident-resource-and-remediation-report.md), the owner cache keeps, with
  the revision, the UID its `get` returned, and the fact slot gives `revision-lookup-failed` when it
  differs from the UID in the Pod's `ownerReference`: the cache is keyed by name, so a ReplicaSet
  recreated under the same name could otherwise lend the Incident the deleted one's revision under
  the new one's UID. Both reviewers asked for it. The revision and the UID sit behind one pointer of
  the cache entry, empty with incidents off.
- In [0028](../adr/0028-incident-resource-and-remediation-report.md), kausa never sends a create
  the schema would refuse: a revision that does not fit is `revision-lookup-failed`, and any other
  value read from the cluster that does not fit means no create, counted as a refusal. The rule is
  the ADR author's.

### Next

- The implementation, behind `incidents.enabled` and `incidents.reportRemediations`, both off by
  default, with the reference pages that 0028 lists under Consequences.
- `podsSeen` is 0 for an incident with no Pod, which the Incident's 1..100 refuses on both sides:
  open, for the maintainer, with kurita's ADR 0008.
- Measure bars 1 to 8, and settle what "Not verified" lists, first whether a metadata `get` of a
  ReplicaSet returns its annotations.
