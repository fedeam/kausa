# 0026. Report every incident as facts without a model, keep the explanation optional, and widen coverage one bounded source at a time

- Status: accepted
- Date: 2026-11-19
- Amends, when accepted: [0001. Build a small, read-only explainer that writes one redacted
  explanation per incident](0001-scope-and-positioning.md) (what kausa is: a reporter of incidents
  whose explanation by a model is optional), with an `Update` paragraph there
- Keeps: 0001's rejection of a dashboard or chat interface, and that kausa never acts on the
  cluster; [0002. Write kausa in Rust with kube-rs and license it MIT OR
  Apache-2.0](0002-rust-and-dual-license.md); [0008. Send only allowlisted, redacted data to one
  OpenAI-compatible endpoint](0008-data-leaving-the-cluster.md) and [0009. Run with read-only
  permissions that never include Secrets](0009-least-privilege-rbac.md) for stages 1 and 2 below
- Decides a direction and its rules. Each stage below still needs its own ADR, with its exact
  format, permission and measured memory, before any code.

## Context

Status as of 2026-11-19 (`main` at `b8b245e`, v0.6.2 released).

**What the maintainer asked for** (FedeAM, 2026-11-19): kausa should make any incident in a
cluster easy to see and report, through the channels it already has (Events, the log, the
optional webhook and the optional Explanation history), not through a new interface. It has to
stay very small, and it must not state anything about an error that is not true. The language
model should be a feature a user turns on, not what kausa is.

**What kausa sees today.** Three watches: Warning events about Pods, Jobs, workload controllers,
PersistentVolumeClaims and HorizontalPodAutoscalers; Pod status; and Deployments. Fourteen
detections (`D001` to `D014`, [Detection and redaction rules](../reference/rules.md)). A Warning
event that no detection matches is dropped, and so is one whose object is in no namespace
([Architecture](../explanation/architecture.md)). Nodes, the logs of containers that are not
failing, and anything a component reports only in its own log are not read.

**What kausa reports without a model.** Without `--endpoint` or `--dry-run`, "kausa only logs
incidents" ([Command line and log format](../reference/command-line.md)): the `incident` line,
with the detection, the owner, the Pods and one sample (container, reason, exit code). No Event
is written, and `--notify-url-file` and `--history` are usage errors without `--endpoint`. So
detection, the settle window, correlation and the fingerprint already run without a model, and
their result reaches only the log. Every channel a person reads carries the model's text or
nothing.

**What a model can and cannot guarantee.** The stages before the model are deterministic: "the
same cluster state produces the same incident, the same fingerprint and the same request bytes"
(Architecture). The model's answer is validated, redacted and checked for six command shapes
([0025](0025-withhold-destructive-commands.md)), and can still be wrong: in 0025's measurement, 9
of 12 answers gave the cause a log line told the model to give. [SECURITY.md](../../SECURITY.md)
says so ("A misleading explanation is still possible"). No check on the answer makes it true.

**One observation of the gap** (2026-11-19, read-only, once, on one small k3s cluster; an
illustration, not a measurement). Over 72 hours of the logs of 15 system components: one sidecar
logged the same list error thousands of times, a DNS server logged an error for a few hundred
queries, and a container had restarted 21 times with its last terminations ending in exit code
`0`, which `D001` leaves out (`Completed`). The cluster held no Warning event at that moment, and
kausa reported nothing: none of this is a Warning event or a failing Pod status.

**Memory.** About 2 MiB with 96 Pods on a Raspberry Pi 4 ([Memory and CPU on a Raspberry Pi
4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)) and 31 MiB with 10,000 Pods
([Thousands of Pods](../research/2026-10-28-thousands-of-pods.md)), because each watch keeps a
small record and not the object. Memory grows with what kausa keeps, by one record each.

**Tokens.** Status as of 2026-11-19, a model call is made once per incident, and an incident is
one workload with one detection: three crashing replicas are one call. The fingerprint hashes
the rule, the namespace, the owner and a normalized message, and keys a cache of up to 1024
explanations kept for `--dedup-hours` (6), so a repeat costs no call. Calls are capped at 30 an
hour and 200 a day, and a request at 6000 estimated input tokens. What is spent can already be
read: `kausa_model_tokens_total` (`in`, `out`), `kausa_model_calls_total`,
`kausa_explanations_cached_total` and `kausa_budget_calls_remaining` on `/metrics`, and
`tokens_in` and `tokens_out` in each `explanation` log line. Two limits: the counters start over
when kausa restarts, and no cap is expressed in tokens. And one gap: the same cause in a hundred
workloads (a registry that is down) is a hundred fingerprints, so a hundred calls, spread by the
hourly cap.

**Features chosen at install.** The chart already works this way: `logs`, `events`, `rollouts`,
`history.enabled`, `notifications.enabled` and `replicaCount` each add a flag and the permission
it needs, and leave both out when off ([Helm chart](../reference/helm-chart.md)).

**The field** ([Competitive landscape](../research/2026-10-23-competitive-landscape.md), not
rechecked for this ADR). Reporting failures without a model exists: kwatch sends failure
notifications with correlation and has "no language model, by design" (42 MB for arm64, Elastic
License 2.0, reads Secrets unless turned off); k8sgpt's analyzers are deterministic; Robusta puts
deterministic triggers in front of HolmesGPT. A mode without a model is therefore not new by
itself.

## Alternatives considered

- **Keep 0001 as it is**, with the model at the centre. Then a cluster with no endpoint gets
  only log lines, and the one output a person reads is the part that can be wrong.
- **See everything at once**: a reflector per kind, or a log stream per container. Memory and
  connections then grow with the cluster, and the role needs `pods/log` on every Pod and reads on
  many more kinds. It gives up both the footprint and 0009.
- **Rewrite in C, Zig or Go to save memory.** The measured memory is the records kausa keeps,
  which a rewrite keeps too. Rust already has no garbage collector and no runtime to shrink. C
  gives up memory safety in code that parses text anyone who can run a Pod controls (0002), and
  has no maintained Kubernetes client. Go was rejected in 0002 for its larger images and memory.
  Where the idle memory goes was not measured for this ADR; that measurement comes before any
  such change is considered again.
- **Make the model's answer trustworthy** with a second model that judges it, or a check of the
  cause against the evidence. 0025 found the cause cannot be checked against the evidence, and a
  judge is one more model that can be wrong.
- **A dashboard.** Still rejected, as in 0001; the maintainer chose the existing channels on
  2026-11-19.
- **A place in the control plane** (an admission webhook, an aggregated API). kausa would then
  be able to break what it watches. Rejected: see point 10 of the Decision.

## Decision

Proposed to FedeAM on 2026-11-19; not accepted yet.

1. **Facts first, in every channel.** Each settled incident is reported as a fact statement
   whether or not a model is configured. Without `--endpoint`, kausa writes one Event on the
   top-level owner (reason `Detected`, proposed) and, if set, posts to the webhook; today's
   usage errors for `--notify-url-file` without `--endpoint` go away. With `--endpoint`, the
   model's explanation is added to the facts and never takes their place: an incident whose
   explanation is skipped is still reported.
2. **What a fact statement is.** Fixed text chosen by the detection, plus values copied from the
   API objects through an allowlist: names already visible on the object, enumerated reasons,
   numbers and times. No free text from a status message, an event note or a log line, and
   nothing a model wrote. The same cluster state gives the same bytes.
3. **What kausa never says.** That something is healthy, that nothing is wrong, or that it saw
   everything. No report means only that no rule matched what kausa watches. What it watches and
   what it does not is a reference page, and whatever a bounded table dropped is counted in a
   metric, so a report never implies more than was seen.
4. **The model's text is marked as a model's.** Every channel shows which lines are facts and
   which are the model's explanation. The format of the `Explained` note, which today is all
   model text in 1024 bytes, is decided in stage 1's ADR.
5. **Coverage grows by stages, cheapest permission first.** One ADR each:
   - Stage 1: the fact statement for the fourteen detections. No new permission; nothing new
     leaves the cluster unless a webhook is set.
   - Stage 2: Warning events that no detection matches. One that repeats past the settle window
     is reported as a fact (kind, reason, reporting controller, count), with no interpretation.
     No new permission.
   - Stage 3: Nodes (conditions such as `Ready` and the pressure ones, and Events about them).
     Needs `list` and `watch` on `nodes`, a cluster-scoped grant the namespaced mode cannot have.
   - Stage 4: the logs of running containers, off by default and per listed namespace: one
     container at a time, a bounded tail, a fixed pace, counting lines by pattern. Needs
     `pods/log` beyond failing Pods, so it amends 0008 and 0009.
   - Not planned: scraping metrics, reading Secrets or ConfigMaps, and any write but kausa's own.
6. **Memory is bounded by design, per source.** A new source keeps a table of at most `N` records
   of at most `B` bytes, drops the least recently seen and counts the drops; anything read in
   bulk (a log tail, a list page) goes through one buffer of fixed size, one item at a time. The
   starting proposal, in line with the bounds kausa has (a cache of 1024 explanations, queues of
   16, messages cut at 1024 bytes): 1024 records of at most 512 bytes for stage 2, so at most
   0.5 MiB, keyed by namespace, kind, reason and reporting controller; and for stage 4 one buffer
   of 64 KiB and 256 records. These numbers are not measured. Each stage's ADR states its `N`
   and `B`, and a research note measures the stage on a Raspberry Pi 4 and with 10,000 Pods
   before the ADR is accepted, as 0001 required for v0.1.
   - **`N` is a default the user can raise.** Each table's `N` is a flag and a chart value, within
     a range its stage's ADR states; `B` stays fixed, so the most a table can take is `N` times
     `B` and can be read from the configuration (65,536 records would be 32 MiB). A table that
     drops records says so in its metric, which is the signal to raise `N`.
   - **Raising it restarts kausa, like any other setting**, and the tables live in memory. What
     comes back and what does not is stated per stage: for stage 2 the list at start brings the
     Warning events the API server still holds (one hour by default), each with its own count,
     so the table is rebuilt from them (a hypothesis until stage 2 is designed).
7. **Each stage is a feature chosen at install, and one that is off costs nothing.** Off means
   no watch, no permission in the rendered role and no table: the footprint of an install is the
   sum of what it turned on. Stages 3 and 4 are off by default. One image holds every feature;
   separate builds are not planned.
8. **Facts cost no tokens, and the new sources never call the model by themselves.** A fact
   statement is built without a model. What stages 2, 3 and 4 report is facts only, unless that
   stage's ADR decides otherwise with a measured cost. The model stays for the fourteen
   detections, within the call budget and the deduplication of today. Two additions are left to
   stage 1's ADR, each to be measured first: one call for the same cause across workloads, and a
   cap in tokens beside the caps in calls.
9. **Rust stays** (0002).
10. **kausa stays an observer.** It is never in the path of a request to the API server, and
   nothing in a cluster depends on it: if kausa stops, only its reports stop. "Safe to install on
   every cluster" is the aim; "critical component" is not.

## Consequences

- **The description changes.** From "explains failing workloads with an LLM" to reporting
  incidents as facts, with an optional explanation by a model. When this is accepted: the README,
  [the documentation front page](../index.md), the chart's description and the Artifact Hub entry,
  and any listing elsewhere.
- **It is less distinct, not more, on the model's side**, and overlaps with kwatch on the facts
  side. What stays particular is the combination: a few MiB, a role without Secrets, the report
  as an Event on the failing object, and a model that is optional, redacted and budgeted. 0001's
  expectation of low adoption stands.
- **More Events.** An incident with an explanation gets a fact statement too, unless stage 1
  puts both in one note. Events are kept for an hour by default, as today.
- **Stage 2 widens what is reported to whatever any controller marks as a Warning**, including
  noise. It needs the settle window, the bounded table and a way to leave reasons out.
- **Stage 4 is the expensive one**, in permission and in load on the kubelets, and may be
  rejected by its own ADR. Without it, an error that a component only writes to its log stays
  unseen, and point 3 makes kausa say so.
- **Pod records still grow with the cluster** (31 MiB at 10,000 Pods). Point 6 bounds the new
  sources, not the existing ones.
- **Point 7 opens the door to features that are not about incidents**, such as reporting
  resource requests against use for cost. That one is not decided here: it needs a read of the
  metrics API, a research note on a field that already has dedicated tools, and its own ADR.
- **Not verified:** nothing here is implemented or measured. The observation of 2026-11-19 is one
  cluster, once. The bounds of point 6 are a design rule until each stage measures them.
- **When accepted:** an `Update` paragraph in 0001, and for each stage the pages its ADR names
  ([Permissions](../reference/permissions.md), [Data sent to the
  provider](../reference/data-sent-to-the-provider.md), [Data sent in
  notifications](../reference/data-sent-in-notifications.md), [Security
  policy](../../SECURITY.md), [ARCHITECTURE.md](../../ARCHITECTURE.md)).

Update 2026-11-19: FedeAM accepted this ADR on 2026-11-19, after the pull request that proposed it
([#115](https://github.com/fedeam/kausa/pull/115)); the sentence that opens the Decision is from
before. Nothing is implemented yet, so the description (the README, the documentation front page,
the chart's description and the Artifact Hub entry) changes with the release that ships stage 1,
not now: until then kausa reports nothing without a model, and the new description would say what
it does not do. 0001 gets its `Update` paragraph with this acceptance.

Update 2026-11-24: [0029. Report when kausa stopped seeing an incident's failure, and whether the change of a proposed action was observed, never that the incident was resolved, off by default](0029-incident-end-report.md) is proposed, not accepted. It keeps point 3 as worded and adds to it:
kausa never says that an incident was resolved, fixed or recovered. What it may say of an incident's
end is two typed statements about its own observation, that it stopped counting the detection's
signal (on a stated basis, or because it lost the permission to see it) and whether a typed field of
the owner's Pods changed as an accepted action would change it, within a stated window; a test keeps
the words `healthy`, `fixed`, `resolved`, `recovered`, `ok`, `success` and `worked` out of every
value and fixed text. This takes effect only when 0029 is accepted, which needs its bars measured
and 0028 accepted; until then the text above holds.
