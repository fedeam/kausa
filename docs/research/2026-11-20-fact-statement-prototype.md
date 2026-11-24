# The fact statement prototype, measured

What a prototype of [0027. Report each settled incident as a fact statement in its own `Detected`
Event and webhook message, with or without a model, and mark the model's text as a
model's](../adr/0027-fact-statement-for-the-detections.md) costs against the three bars of that
record's point 10, and what its hypotheses about the API server turned out to be. The three bars
hold in kind and on a Raspberry Pi 4; two of the record's numbers and one of its hypotheses do
not.

Measured on 2026-11-20 in a kind cluster on the maintainer's machine, and on 2026-11-24 in a k3s
cluster of Raspberry Pi 4 ([On a Raspberry Pi 4](#on-a-raspberry-pi-4)), each figure from two to
ten runs: a data point, not a benchmark.

## Result

| Bar of point 10 | Measured | Holds |
|---|---|---|
| Idle memory within the run-to-run spread of v0.6.2, 10,000 Pods | The process's own memory in all ten runs; the `docker stats` figure in nine of ten | Yes, with the note below |
| A full fact queue adds at most 2 MiB | 1.73 to 1.74 MiB resident (1.55 MiB computed) | Yes, by 13% |
| 1024 `Detected` Events in a row cause no watch error and no relist | 0 errors, 0 relists, 0 restarts on the three watches, twice | Yes |
| The same three on a Raspberry Pi 4, about 100 Pods | Within the spread; 1.80 MiB; 0 errors, 0 relists, three runs | Yes |

Idle memory with 10,000 Pods, 20 seconds after start, ten runs of each image, in MiB:

| | v0.6.2, published image | `main` at `a5a624a` | Prototype |
|---|---|---|---|
| `docker stats`, lowest to highest | 30.80 to 31.58 | 30.38 to 31.61 | 30.64 to 32.72 |
| The same figure from the API, median | 31.49 | 31.45 | 31.49 |
| Anonymous memory, lowest to highest | 29.72 to 30.85 | 29.73 to 30.77 | 29.80 to 30.73 |
| Anonymous memory, median | 30.68 | 30.69 | 30.67 |

- **Idle memory does not change.** The anonymous memory of the container's control group, which
  is the process's own, has the same median in the three images to 0.02 MiB, and every run of
  the prototype is inside the range of v0.6.2. In `docker stats` nine runs of the prototype are
  inside 30.64 to 31.60 MiB and one reads 32.72 MiB, 1.14 MiB over the highest of v0.6.2, with
  its anonymous memory at the usual level (30.73 MiB). The image of `main`, which has none of
  the prototype's code, showed the same kind of excess 25 seconds earlier (34.20 MiB at its 60
  second reading). That the excess is memory of the kernel charged to the container while the
  host was busy is a hypothesis: its breakdown was not captured.
- **A full fact queue is 1.55 MiB computed and 1.74 MiB resident**, not the 1.3 MiB of the
  record. One entry at the longest names Kubernetes allows is 1,583 bytes: 1,001 of names, 208 of
  the entry itself and 374 of the statement's buffer, which holds a statement of 269 bytes. The
  resident memory of the process (`VmRSS`) grew by 1,776, 1,776 and 1,784 kB in three runs when
  the queue filled, and by 40 to 60 kB more when the table of notified fingerprints filled (40
  bytes an entry, 40 KiB computed).
- **Writing 1024 Events takes about a second and disturbs nothing.** One at a time, the 1024
  creates took 954 and 975 ms; `kausa_event_write_errors_total`, `kausa_facts_dropped_total` and
  the three counters of each watch (`kausa_watch_errors_total`, `kausa_watch_relists_total`,
  `kausa_watch_restarts_total` for `pods`, `events` and `deployments`) read 0 before, right after
  and 60 seconds after.
- **The longest statement is 286 bytes**, not the "about 310" of point 1: a unit test pins it.

What the numbers ask of the record before acceptance:

- Point 1: 286 bytes for the longest statement.
- Points 3 and 10: 1,583 bytes an entry and 1.55 MiB for the full queue, computed, and 1.74 MiB
  resident; the bar of 2 MiB stays. The table of point 5 is 40 bytes an entry, inside its "under
  64 bytes".
- Point 10, the first bar: name the figure. The process's own memory separates the images to
  0.02 MiB; `docker stats` moves by more than that between two readings of one idle container.
- Point 10, the hypotheses: the API server does not accept any image string (see below).

## Setup

- kind v0.33.0 with Kubernetes v1.36.4 (the node image pinned in `.github/workflows/e2e.yml`), in
  a Linux virtual machine of 16 CPUs and 7.65 GiB, arm64.
- The cluster of [Thousands of Pods](2026-10-28-thousands-of-pods.md): a Node without a kubelet
  and 20 namespaces of 50 Deployments of 10 replicas, written by
  [`tests/bench/thousands-of-pods.py`](../../tests/bench/thousands-of-pods.py). 10,000 Pods
  pending, 1,002 ReplicaSets, 1,002 Deployments.
- Three images, all of them reporting `kausa 0.6.2`: the published `ghcr.io/fedeam/kausa:0.6.2`
  (arm64); `main` at commit `a5a624a`; and the prototype, which is that commit plus the changes
  of the unmerged branch `proto/0027-fact-statement`. The last two were built with the commands
  of the `build` stage of the `Dockerfile` in a container of its pinned Rust image (1.99.1), and
  copied onto the base image of the `runtime` stage with the same user, entry point and command.
  They were not built with `docker build --target runtime`, because the disk of the virtual
  machine had no room for the layers of the build. `main` built this way is the control: it
  reads the same as the published image, so the way of building does not move the figures.
- Each run: a new container of the image on kind's Docker network, with a kubeconfig, without an
  endpoint, `run --metrics-address 0.0.0.0:9095`; readings 20 and 60 seconds after start; then
  the container is removed. The three images alternated, ten rounds.
- Memory: `docker stats --no-stream`, as in the earlier note, and the same instant from the
  Docker API, which gives the usage without the inactive file cache (what `docker stats` shows)
  and the anonymous memory. For the queue, also `VmRSS` of `/proc/self/status`, logged by the
  process around the filling.
- The prototype has `src/fact.rs` (point 1), the `Detected` Event (point 2), the fact queue of
  1024 that drops the oldest and its own slot (point 3), the table of 1024 notified fingerprints
  (point 5), and of point 8 the `statement` field of the `incident` line, the `fact dropped`
  line, `kausa_facts_dropped_total`, `kausa_fact_reasons_omitted_total` and
  `kausa_events_written_total{reason="Detected"}`.
- A harness inside the prototype, switched on by the environment variable
  `KAUSA_PROTOTYPE_0027_HARNESS` and not meant to be merged: `fill:<seconds>` fills the queue
  and the table with 1024 entries of the longest names and holds them;
  `events:<seconds>:<namespace>` queues 1024 facts of 1024 Deployments that do not exist, which
  the ordinary path then writes as Events.
- The hypotheses about names and images: `kubectl create --dry-run=server` of a Pod for each
  value.

## Observations

- **A cluster like this one has 1,000 incidents of its own.** The Pods never become ready, so
  each Deployment is past its progress deadline (`D014`) and settles 60 seconds after start.
  The idle readings at 20 and 60 seconds are from before that. In the two runs of the Event
  harness the prototype then wrote those 1,000 statements too ("rollout does not progress
  (D014). Sample: reason ProgressDeadlineExceeded. Revision: 1."): 1,000 incidents within 86 ms,
  no fact dropped, 2,024 and 2,027 Events written in each run with no write error and no watch
  error. The second run found the Events of the first and patched their series (`count: 2`)
  instead of creating new ones. Anonymous memory after that was 31.17 and 31.25 MiB, 0.45 to
  0.47 MiB over the same container at 25 seconds.
- **Three real failures gave three statements.** A container that exits with 3, an image that
  does not exist and a command that does not exist, one Deployment each:
  "crash loop (D001). Sample: container app, reason Error, exit code 3. Pods seen failing: 2.
  Pods now: 2." (103 bytes), "image cannot be pulled (D002). Sample: container app, reason
  ErrImagePull. Pods seen failing: 1. Pods now: 1." (109 bytes) and "container cannot be created
  or started (D004). Sample: container app, reason RunContainerError, exit code 128. Pods seen
  failing: 1. Pods now: 1." (145 bytes). Each became a `Warning` Event of reason `Detected`,
  action `Detect`, reporting controller `kausa`, regarding the Deployment and related to the
  sample Pod. `kausa_fact_reasons_omitted_total` stayed at 0.
- **`events.k8s.io` accepts the reason `Detected` and the action `Detect`**, and an API server
  with default admission refused none of the 1024 creates sent one at a time, in either run.
  Verified on Kubernetes v1.36.4 only.
- **The API server refuses a container name that is not a DNS label.** Refused: upper case, an
  underscore, a dot, a space, a leading or trailing `-`, 64 bytes, `:` and `/`, a non-ASCII
  letter, a newline. Accepted: 63 bytes, a digit first. Verified by behaviour on v1.36.4, not in
  the API server's code.
- **The API server does not accept any image string**: it refuses an empty one and one with
  leading or trailing whitespace. It accepted everything else tried: upper case, spaces inside,
  markup, a newline inside, a tab and a control byte, non-ASCII letters, 4,096 bytes and 200,000
  bytes. The record's reason for keeping the image out of the statement stands.
- **Spare capacity moves the queue's figure by a tenth.** A first version of the harness built
  the names with `format!`, which left them room they did not use: 1,772 bytes an entry and 1.90
  to 1.91 MiB resident. Real entries hold clones of the incident's names, which have no spare
  room; the harness was changed to match and the figures above are from that version. The
  statement keeps the 105 spare bytes its building leaves it.
- **`docker stats` cannot resolve the second bar.** Between the reading before the filling and
  the one after, it rose by 1.81, 2.27 and 1.94 MiB as displayed, and by 2.14, 1.96 and 2.00 MiB
  in the same figure read from the API. Between the same two readings of an idle container that
  figure moved by -1.34 to +0.41 MiB, and by +2.95 MiB in the outlier of `main`. The anonymous
  memory of the control group rose by 1.80, 1.80 and 1.84 MiB, queue and table together, and by
  -0.04 to +0.08 MiB in the idle runs of v0.6.2 and the prototype.
- **Anonymous memory at rest has two levels** in the three images, about 29.8 and about 30.7
  MiB. Why is not known; it is not the prototype's.

## On a Raspberry Pi 4

Measured on 2026-11-24. The three bars hold there as well.

Point 10 of the record repeats 0026's "on a Raspberry Pi 4 and with 10,000 Pods". This note reads
it as two measurements: 10,000 Pods in kind, above, and a real cluster of Raspberry Pi 4 with the
Pods it runs, here. That reading is an interpretation, not something the record says.

### Setup

- A k3s cluster of three Raspberry Pi 4 Model B (8 GB, arm64), not kind, with about 100 Pods.
- Two Pods side by side on the same worker node, in a namespace of their own that was deleted
  afterwards: the published v0.6.2 image, by digest, and an arm64 image of the prototype built
  from the branch `proto/0027-fact-statement` at `fda453a`.
- Both from chart 0.6.2 with the same values: `rbac.scope=cluster`, `settleSeconds=3600`,
  `endpointCheck.enabled=false`, one replica, and no endpoint, no history and no notifications.
- Chart 0.6.2 grants Event writes only with an endpoint, so for bars 2 and 3 the prototype got a
  Role and RoleBinding of `create` and `patch` on Events in that namespace only.
- Memory: the container's `rssBytes` and `workingSetBytes` from the kubelet's summary API. That
  `rssBytes` is the anonymous memory of the control group, the figure point 10 names, is a
  hypothesis: it was not checked on these nodes. The kubelet's timestamp for a reading was 11 to
  19 seconds behind the clock, so a reading "at 60 seconds" is from 41 to 49 seconds after start.
  For the queue, also `VmRSS` of `/proc/self/status`, logged by the process, as in kind.
- Counters: `/metrics` of each Pod, through the API server's proxy.
- The harness of the kind measurement: `fill` for bar 2, `events:120:<namespace>` for bar 3.

### Bar 1: idle memory, ten rounds

Both Pods restarted each round, read at 20 and 60 seconds; lowest, median and highest, in bytes:

| | v0.6.2 | Prototype |
|---|---|---|
| `rssBytes` at 20 s | 1,368,064 / 1,439,744 / 1,642,496 | 1,318,912 / 1,466,368 / 1,630,208 |
| `rssBytes` at 60 s | 1,380,352 / 1,445,888 / 1,642,496 | 1,318,912 / 1,482,752 / 1,630,208 |
| `workingSetBytes` at 60 s | 1,617,920 / 1,734,656 / 2,404,352 | 1,556,480 / 1,728,512 / 1,998,848 |

Every reading of the prototype is at or under the highest of v0.6.2; its lowest is under the
lowest of v0.6.2, and its median at 60 seconds is 36,864 bytes over. In the ten rounds, in both
Pods, `kausa_watch_errors_total`, `kausa_watch_relists_total` and `kausa_watch_restarts_total`
read 0 for `pods`, `deployments` and `events`, and `kausa_event_write_errors_total` and
`kausa_facts_dropped_total` read 0, with no restart.

### Bar 2: a full fact queue, three runs

- The harness logged 1024 entries of 1,583 bytes, 1,620,992 bytes in all.
- `VmRSS` grew by 1,844, 1,844 and 1,848 KiB when the queue and the table filled, under the
  2,048 KiB of the bar; the kubelet's `rssBytes` by 2,011,136, 1,826,816 and 1,830,912 bytes,
  against 2,097,152.
- No `event write failed` line.

### Bar 3: 1024 `Detected` Events in a row, three runs

Each run deleted the Events of the namespace, waited, restarted both Pods, read the counters 60
seconds after start, let the harness queue its 1024 facts at 120 seconds, and read again when the
queue was empty and 60 seconds after that.

| Run | `Detected` Events before and after | Queue emptied in |
|---|---|---|
| 1 | 1,024 and 1,024: an earlier Pod had run the harness during the deletion, so the writes patched existing Events | 21,487 ms |
| 2 | 0 and 1,024 | 7,906 ms |
| 3 | 0 and 1,024 | 10,206 ms |

- In the three runs, at the three readings, in both Pods: `kausa_watch_errors_total`,
  `kausa_watch_relists_total` and `kausa_watch_restarts_total` 0 for `pods`, `deployments` and
  `events`. The prototype: `kausa_events_written_total{reason="Detected"}` 1024,
  `kausa_event_write_errors_total` 0 and `kausa_facts_dropped_total` 0.
- No `event write failed` line, no restart, and no `warn` or `error` line in either Pod's log.
- 60 seconds after the writes, `rssBytes` and `workingSetBytes` of the prototype were 1,703,936
  and 2,469,888, 1,818,624 and 2,060,288, and 1,642,496 and 1,884,160 bytes; of v0.6.2 at the
  same moments, 1,523,712 and 1,761,280, 1,462,272 and 1,970,176, and 1,507,328 and 1,748,992.
  No bar is about this figure.

**A first version of this procedure was discarded.** It deleted the Events with both Pods
watching and read the counters after the harness had started. In it the prototype showed
`kausa_watch_errors_total{watch="events"}` 2 and `kausa_watch_relists_total{watch="events"}` 2.
Both errors came 20 seconds before the harness queued anything, and their text was the API
server's answer to the deletion: `etcd event received with PrevKv=nil` for a `DELETE` of an
Event, an `InternalError`. On k3s with kine, deleting Events while a watch on Events is open gives
that error to any client that watches them, v0.6.2 included; writing Events did not cause it.
The three runs above delete, wait and restart before they read.

## Limits

- **The two measurements are not one.** The 10,000 Pods were measured in kind only; the Raspberry
  Pi 4 ran about 100 Pods. Reading point 10 as two measurements is this note's interpretation.
- On the Raspberry Pi 4 the figures are the kubelet's, read 11 to 19 seconds late, and
  `VmRSS`; the anonymous memory of the control group was not read there directly.
- Chart 0.6.2 and an extra Role, not the chart that ships with the implementation.
- One machine, a local API server, and a virtual machine shared with other containers. The
  outliers of `docker stats` were not explained.
- The ten idle rounds ran the prototype's binary from before the change to the harness's names;
  the binary rebuilt after it was read five times at rest (anonymous memory 30.66 to 30.80 MiB,
  inside the range of v0.6.2), not ten.
- Memory after the 1,000 `D014` incidents settle was read for the prototype only, not for v0.6.2
  or `main`, so the cost of holding and writing real facts at that scale has no baseline here.
- The entries of the full queue are synthetic. Their names are the longest Kubernetes allows,
  which no real workload has; their statement is the longest of `D004` (269 bytes), 17 bytes
  under the longest possible.
- Not in the prototype: the `detected` webhook message and its limit per hour, the mark on the
  model's text (point 4), the chart and its RBAC rule, the fuzz target, the reference pages and
  an end-to-end test. The table of notified fingerprints is filled after the Event is written,
  since there is no webhook path to mark it; its memory is the real one, its behaviour is not
  measured. The cost of the webhook path is not in any figure of this note.
- Which terminated reasons other than `Error` the restart-count path of `D001` meets remains a
  hypothesis: it was not read in the kubelet's or containerd's code. The one data point here is
  that a command that does not exist was `D004` with `RunContainerError`, not `D001`.
