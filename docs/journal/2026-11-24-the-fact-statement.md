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
