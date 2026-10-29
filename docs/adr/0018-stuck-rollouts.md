# 0018. Explain a Deployment past its progress deadline (`D014`) from a Deployment watch that is on by default, and add nothing for StatefulSets or DaemonSets

- Status: proposed
- Date: 2026-10-29
- Builds on: [0001. Build a small, read-only explainer that writes one redacted explanation per
  incident](0001-scope-and-positioning.md) and [0016. Send each Pod's status reason and message to
  the model, redacted and bounded](0016-send-the-pod-status-reason-and-message.md) (unchanged)
- Amends: [0009. Run with read-only permissions that never include
  Secrets](0009-least-privilege-rbac.md) ("nothing on Deployments, StatefulSets, DaemonSets or
  CronJobs"), [0013. Offer a namespaced install mode: one watch pair and one Role per listed
  namespace, and nothing cluster-wide](0013-namespaced-install-mode.md) (three watches per
  namespace) and [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md) (one object of integers). No invariant of
  [ARCHITECTURE.md](../../ARCHITECTURE.md) changes in wording.
- Related: `0017-richer-context-for-the-model.md` and `0019-high-availability.md`, the other v0.4
  ADRs; the rules `D011` to `D013`, being implemented for v0.4 on 2026-10-29 with no new permission

## Context

Status as of 2026-10-29 (v0.3.0): kausa watches Pods and Warning Events, and `get`s ReplicaSets and
Jobs only to follow owner references (`src/owner.rs`). Its rules describe Pods, Jobs and, from
v0.4, the controllers that cannot create Pods: `D011` is a Warning `FailedCreate` event about a
ReplicaSet, Job, StatefulSet or DaemonSet, grouped under the Deployment through the existing
ReplicaSet hop. What no rule sees is a rollout whose Pods exist but never become available while
nothing fails: a readiness gate nobody sets, a `minReadySeconds` never reached, Pods held by their
scheduling gates (`D005` ignores `SchedulingGated`), probes that fail too seldom for `D006`.

What Kubernetes offers (source read on 2026-10-29; Kubernetes v1.36.4 on kind is the e2e image):

- **Deployment.** After `spec.progressDeadlineSeconds` without progress, the deployment controller
  sets `Progressing` to `False`, reason `ProgressDeadlineExceeded`, message `ReplicaSet "<name>"
  has timed out progressing.` (`syncRolloutStatus`, `pkg/controller/deployment/progress.go`).
  **It emits no Event**, so the event watch cannot see it. Progress, or a new rollout, sets the
  condition back to `True`. The controller removes it when the deadline is unset or `MaxInt32`
  (`HasProgressDeadline`); the API documents a default of 600 seconds. A paused Deployment gets
  `Unknown`/`DeploymentPaused`, **except** after a timeout: `checkPausedConditions` (`sync.go`)
  does not overwrite `ProgressDeadlineExceeded`. Each rollout bumps the annotation
  `deployment.kubernetes.io/revision` (`SetDeploymentRevision`).
- **StatefulSet and DaemonSet.** No progress deadline and no failure condition. In the
  StatefulSet source read, the alpha `Recreate` strategy (feature gate off by default) sets
  `Progressing` only to `True` (`RecreateInProgress`); a `False` reason was not found
  (hypothesis). `OnDelete` and `partition` rollouts wait by design. Their stalls are failing Pods
  (`D001` to `D010`, grouped under the StatefulSet or DaemonSet) or refused Pods (`D011`).

Pod rules already explain most stalled Deployments, on the same key: `top_owner` stops at the
Deployment, so a crash-looping rollout gives `D001`, and a refused one `D011`, about the Deployment.

## Decision

### The rule

- **`D014`, "rollout does not progress"** (`Detection::RolloutStalled`, after `D013`). It matches a
  `DeploymentRecord` whose `Progressing` condition is `False` with reason
  `ProgressDeadlineExceeded` (exact), whose `spec.paused` is not `true`, and whose
  `status.observedGeneration` is at least `metadata.generation` (both present), so a condition
  older than an edit does not count. Pure: `detect_rollout` in `src/detect.rs`.
- **A state** (`is_state`, `settles`): an incident once present over the settle window, forgotten
  `STATE_GRACE` (330 seconds) after it is gone. No freshness limit. Reads no logs.
- **About the Deployment itself:** owner `apps/v1` `Deployment`, name and UID from its metadata.
- **Fingerprint text `revision <n>`**, `<n>` the revision annotation, kept only when it parses as
  an unsigned integer (else empty), hashed as is, not through `normalize`, which turns digits into
  `#`. One fingerprint per rollout, stable across restarts. The sample's message is that text,
  logged in the `incident` line; it is never sent.
- **Nothing for StatefulSets or DaemonSets**, and no permission on them.

### One explanation per stall

- **`D014` yields only to an explained incident.** `Tracker::tick` decides every other key first
  and the `D014` keys after them, so the `BTreeMap` order of the keys does not matter. Then a
  `D014` entry not yet emitted, with another `pending` key of the same namespace, owner kind and
  owner name, is:
  - **handled**, without an incident, when one of those keys is `emitted`, including one that
    settled on this tick. A handled `D014` stays silent while its signal lasts, and `STATE_GRACE` after.
  - **deferred** for this tick, neither emitted nor handled, when no such key is `emitted`: an
    event detection still fresh, or a state within its settle window or its `STATE_GRACE`. Its own
    settle window keeps running, so it is emitted on the first tick the other is gone.
  So `D014` is silent only when another incident about the Deployment was explained, and is
  explained once. A signal that never settles delays it instead: one Warning `Unhealthy` event
  (any is a `D006` signal for `EVENT_FRESHNESS`, 10 minutes, `src/run.rs`, and an event settles
  only when one series spans the window, `Tracker::settled`) until 10 minutes after the event, a
  `D005` that clears within the window until `STATE_GRACE` (330 seconds) after it clears.
- **Residual risk.** Whoever can create Warning Events about the Deployment's Pods in its
  namespace, or make a Pod's probe fail, can delay `D014` for as long as they keep producing
  fresh, distinct events that never settle; it comes 10 minutes after they stop. The same writer
  can already forge explanations with events, and an explained forgery is itself visible. With
  `D011`, the same holds for events about its ReplicaSets (hypothesis: `D011` is event-based; it
  is not in this tree).
- **One direction.** A Pod rule or `D011` that starts after `D014` was emitted is still explained.
- A Pod whose ReplicaSet lookup failed is grouped under the ReplicaSet: it does not hold back
  `D014`, and both are explained.

### The Deployment watch

- **On by default.** `kausa run --no-rollouts` turns it off (help: `  --no-rollouts          Do not
  watch Deployments for stalled rollouts (D014)`). Chart value `rollouts` (boolean, default `true`),
  read so that a missing value (v0.3 values reused) means `true`; `false` passes `--no-rollouts`
  and removes the grant. One value drives both.
- **A watch**, on `Api<Deployment>` through the existing `watches` function: `Api::all`, or one
  `Api::namespaced` per `--namespace`, with `Restarting`, the default backoff and
  `watcher::Config::default()` (pages of 500, as for Pods). kube-runtime 4.2.0's watcher calls only
  `list` and `watch` (`src/watcher.rs`, read on 2026-10-29).
- **Reduced at once** (invariant 8). `DeploymentRecord::from_deployment` (`src/record.rs`) keeps:
  namespace, name, UID, `generation`, `observedGeneration`, `paused`, the revision (as above),
  `progressDeadlineSeconds`, `minReadySeconds`, `spec.replicas`, the status counts `updated`,
  `ready`, `available` and `unavailableReplicas` (absent is `0`), and the `Progressing` condition's
  status and reason (`bounded`). Never kept: the Pod template, labels, other annotations, managed
  fields, the selector, the strategy, any condition message, `ReplicaFailure`.
- **Only matching records are kept.** `State` keeps a record while `detect_rollout` matches, and
  drops it on any update that does not, and on delete. Memory grows with stalled Deployments.
- **As in 0013:** per-watch relists, a `403` drops that watch's records and restarts the watcher,
  other errors keep them; `/readyz` turns `200` once every watch has listed: 3 (or 2 with
  `--no-rollouts`), or 3 × N; an ended watch stops kausa with exit code `1` and `the deployment
  watch ended` (`… of namespace <name> ended`).
- **Log and metrics:** `started` gains `rollouts` (`true`/`false`); `deployment watch error`
  (`namespace`, `error`); `kausa_watch_errors_total{watch="deployments"}`; `kausa_incidents_total`
  counts `D014`. No new counter, and no metric names a Deployment.

### Permissions

`kausa.rules` gains one rule after `replicasets`, rendered in the ClusterRole and in every Role
while `rollouts` is on:

```yaml
- apiGroups: ["apps"]
  resources: ["deployments"]
  verbs: ["list", "watch"]
```

Never granted: `get` or any write on `deployments`, its `status`, `scale` and `rollback`
subresources, any verb on `statefulsets` or `daemonsets`, `list` or `watch` on `replicasets`.
Secrets and ConfigMaps stay out in every verb. With `rollouts: false` the rules are v0.3.0's.

### Data

- **To the model, for `D014` only:** a top-level `rollout` of integers or `null`:
  `replicas`, `updated_replicas`, `ready_replicas`, `available_replicas`, `unavailable_replicas`,
  `progress_deadline_seconds`, `min_ready_seconds`. No condition message, no `ReplicaFailure`: the
  refusal reaches the model through `D011`'s redacted event notes. `pods` is empty (`pods_total`
  counts the Deployment's Pods kausa holds), `warning_events` keeps its filter, `log_tail` is
  absent. Other requests, the system message, the answer schema and `PROMPT_VERSION` do not change.
- **Event, Explanation and webhook:** no new field; the rule is `D014` (`^D[0-9]{3}$` holds), the
  owner the Deployment, written and named as owners are today.

## Alternatives

- **`get` a Deployment when a Pod names it:** misses the stalls with no failing Pod, the only ones
  `D014` adds. **A metadata watch:** no `status`. **A periodic `list`:** same grant, later, heavier.
- **Off by default (`--rollouts`):** least privilege, but the feature most installs never get,
  for a read close to what `list` on Pods gives. **Always on, no opt-out:** custom RBAC and the
  read-only proxy of the how-to would keep kausa from ever becoming ready.
- **Send the condition and `ReplicaFailure` messages:** free text any writer of
  `deployments/status` sets, and admission errors with names no rule knows; `D011` carries them.
- **Send the Deployment's Pods:** `PodRecord` has no readiness, so the three chosen could be old,
  healthy ones. Readiness for every request is outside this ADR.
- **A separate `FailedCreate` rule (`D015`):** `D011` is that rule.
- **A kausa deadline for StatefulSets and DaemonSets:** no Kubernetes signal, false alarms on
  `OnDelete` and `partition`, two more grants and 2 × N watches. **The `Recreate` condition:**
  alpha, off by default, never `False` in the source read.
- **Fingerprint from the normalized message** (digits become `#`, a ReplicaSet hash becomes `<hex>`
  only with eight or more characters, a digit and a letter: unstable), or **empty** (every later
  stall of the Deployment reuses the first explanation within `--dedup-hours`).
- **`D014` settles once the other incident clears:** `D011`'s events go quiet while the refusal
  lasts (seen on kind), so the same cause would be explained twice. **Pod rules yield to `D014`,**
  or **one merged incident:** the Pod rule names the cause and comes first.
- **Yield to any pending signal of the same owner:** one fresh `Unhealthy` event at `D014`'s first
  ticks, which never settles and is dropped once stale, silenced the stall for good, with nothing
  explained, and gave anyone who can create one Warning event a way to hide it. **Ignore unsettled
  signals** (settle `D014` while they are pending): a crash loop not yet settled, or absent between
  back-offs, would be explained twice once it settles.
- **Smaller pages for the Deployment watch only:** the Pod watch has the same exposure; one issue
  for all three watches. **Also match `ReplicaSetCreateError`:** set at once on any create error,
  transient ones included; left out.

## Consequences

- **Explained:** Deployments stalled with nothing failing. Not seen: a Deployment with no deadline,
  a paused one, a stall shorter than the deadline plus the settle window (about 11 minutes with
  the defaults), and StatefulSets and DaemonSets that wait without a failing or refused Pod
  ([Detection and redaction rules](../reference/rules.md), "Limits"). Delayed: a stall with an
  unsettled signal of the same Deployment, until that signal is gone.
- **A new read.** In the namespaces it covers, kausa, or a stolen token, can list and watch whole
  Deployments: inline environment values of templates, annotations (with
  `last-applied-configuration`), and Deployments scaled to zero that no Pod shows. No Secret or
  ConfigMap. Whoever edits a Deployment can provoke `D014`, and a writer of `deployments/status`
  can forge it: one call per incident within the budget, and no Deployment text reaches the model.
- **Memory.** Records are few and small, but a list page holds up to 500 whole objects, which no
  Pod quota bounds for Deployments with `replicas: 0`, as for Pods today (hypothesis: annotations
  are capped at 256 KiB, an object at about 1.5 MiB). **A condition of shipping, as in 0013:**
  repeat [Thirty-two namespaces](../research/2026-10-28-32-namespaces.md) with 96 watches in a new
  research note before merging; expected about 4 MiB more (130 KiB per watch, hypothesis). If not
  well under 128 MiB, the cap drops to 16.
- **Upgrades.** RBAC made outside the chart, a proxy, or a v0.4 image on the v0.3 chart gets `403`
  on the Deployment watch: kausa keeps explaining but is never Ready (0013's rule), and logs
  `deployment watch error` every 30 seconds at most. The fix is the rule or `--no-rollouts`.
- **First start.** Every Deployment already past its deadline is explained, within the budget and
  the queue of 16 (the oldest dropped and counted); a restart does it again, as for Pod states.
- **`0019-high-availability.md`:** the Deployment watch runs in the leader only, like the other
  two, and counts in its readiness. **`0017-richer-context-for-the-model.md`** adds fields to
  `pods`, which a `D014` request leaves empty, so the two do not interact.
- **Tests.** Unit: the condition matrix of `detect_rollout` (`True`, `Unknown`, paused after a
  timeout, a stale generation, no condition, another reason) and the ids; the record drops the
  template, a fake inline secret and annotations, and parses the revision; the tracker stays silent
  for the whole stall after an emitted `D001` or `D011` of the same owner, including one that
  settles on the same tick, and is emitted after one fresh `Unhealthy` event (count 1, never
  settled) present at its first tick once that event is stale, and after a `D005` that clears
  within the window once it is forgotten; it is not held by another owner, and fingerprints per
  revision; relists,
  `403`, readiness over 3 × N and 2 × N, and the ended watch in `src/run.rs`; the golden
  `rollout` in `src/context.rs`; `--no-rollouts` in `tests/cli.rs`. Chart: the rule in both scopes,
  absent with `rollouts: false` and present with the value missing; the v0.1.0 comparison renders
  with `rollouts=false`. Kind, both scopes: a gated Deployment explained as `D014`; `crasher` and
  a refused Deployment past their deadlines with `D001` and `D011` and no `D014`; nothing in the
  unlisted namespace; `can-i` granted and refused lists; an upgrade with `rollouts=false`.
- **Documentation, in the same change:** [Detection and redaction rules](../reference/rules.md),
  [Permissions](../reference/permissions.md), [Data sent to the
  provider](../reference/data-sent-to-the-provider.md), [Command line and log
  format](../reference/command-line.md), [Helm chart](../reference/helm-chart.md),
  [Architecture](../explanation/architecture.md), [Install kausa with
  Helm](../how-to/install-with-helm.md), [Try kausa against a cluster with a local
  model](../how-to/try-with-a-local-model.md) (the proxy's Deployment paths), [Add a detection or
  redaction rule](../how-to/add-a-rule.md), the [Security policy](../../SECURITY.md),
  `ARCHITECTURE.md`, `NOTES.txt`, the README and `CHANGELOG.md` (`Added`: `D014`, `--no-rollouts`;
  `Changed`: the grant and three watches). On acceptance, `Update` paragraphs in 0008, 0009, 0013.
