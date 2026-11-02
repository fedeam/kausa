# Detection and redaction rules

The deterministic rules kausa applies before any language model call: detection rules decide what
is an incident and how failures are grouped, and redaction rules decide what never leaves the
cluster.

Status as of 2026-11-02: the **detection rules, grouping and fingerprint are implemented** (first
implementation weekend, `src/detect.rs`, `src/incident.rs`, `src/fingerprint.rs`), and so are the
redaction rules and the log reading of the "Logs read" column (second weekend); `D009` and `D010`
and the changes to `D001` and `D005` came on 2026-10-28, and `D011` to `D013` and the new cases of
`R003` on 2026-10-29, all from failures reproduced in kind; `D014`, from a watch on Deployments,
also on 2026-10-29 ([0018. Explain a Deployment past its progress deadline (`D014`) from a
Deployment watch that is on by default, and add nothing for StatefulSets or
DaemonSets](../adr/0018-stuck-rollouts.md)); and on 2026-11-02, from failures reproduced in kind
(Kubernetes v1.36.4), `ErrImageNeverPull` in `D002`, a container that cannot start in `D004`, the
Job's other failure reasons in `D008`, a CronJob in `D011` and a new case of `R003`, with no new id
([below](#the-coverage-additions-of-2026-11-02)). Ids, triggers and
defaults may change until v0.1 is released; after that an id is never reused. How the rules fit in
the pipeline: [Architecture](../explanation/architecture.md). Why redaction works this way: [0008.
Send only allowlisted, redacted data to one OpenAI-compatible
endpoint](../adr/0008-data-leaving-the-cluster.md).

## Detection rules

A detection matches a signal: a **state** read from a Pod at every check (a container state, the
Pod's scheduling condition, or its eviction) or from a Deployment (its `Progressing` condition), or a Warning **event** about a Pod, a Job, a workload
controller (a ReplicaSet, a StatefulSet or a DaemonSet), a CronJob, a PersistentVolumeClaim or a
HorizontalPodAutoscaler. The failure must
persist through the **settle window** (60 seconds by default, `--settle-seconds`) before it becomes
an incident, so a container that restarts once and recovers is not explained:

- **A state** settles when it has been present, at the checks every 5 seconds, over at least the
  window. It is forgotten only after 330 seconds without it, longer than the kubelet's longest
  back-off (300 seconds), so the moments a crash-looping container runs between back-offs do not
  split one failure into several incidents.
- **An event** settles when the same event happens again at least a window after it was first
  seen, by kausa or in its own series (`count` above 1 and a first occurrence a window before the
  last). Single events of different Pods never add up: a rolling update whose new Pods fail one
  readiness probe each is not an incident. An event counts as a signal for 10 minutes after it last
  happened (hypothesis behind the value, to verify on a live cluster: client-go's event recorder
  updates a repeated event about every 5 minutes at most).
- **A failed Job** is final and is an incident at once, and so is a CronJob that cannot create
  its Jobs (a `D011` event whose `regarding` is the CronJob). The CronJob controller writes one
  short series per scheduled time, which would never settle
  ([below](#the-coverage-additions-of-2026-11-02)). A Job of a CronJob whose Pods are refused is
  keyed to the CronJob too, but its events follow the event rule: the Job controller keeps
  retrying while the Job is active.

Pods of the same top-level owner with the same detection form one incident, logged once while it
lasts and again only if it clears and comes back. The incident lists every Pod seen failing while
it was pending, so replicas whose crash loops are out of step all count. Past 100 such Pods (a
workload whose Pods keep being replaced), it keeps the first 100 names in sorted order, plus the
Pods failing when it settles.

| Id | Failure | Signal | Logs read |
|---|---|---|---|
| `D001` | Crash loop | Container waiting with reason `CrashLoopBackOff`, or terminated with any reason but `Completed` after at least one restart (between restarts the kubelet reports it as terminated for much of each back-off), in a Pod that is not in phase `Failed` | Yes: the previous instance while the container runs or waits again, otherwise the terminated one |
| `D002` | Image cannot be pulled | Container waiting with `ErrImagePull`, `ImagePullBackOff`, `InvalidImageName` or `ErrImageNeverPull` (`imagePullPolicy: Never` and an image the node cannot use: absent, or present but not usable with the Pod's credentials) | No |
| `D003` | Out of memory | Container waiting in `CrashLoopBackOff` whose last termination is `OOMKilled`, or terminated with `OOMKilled` and not restarted | Yes, previous container if it restarted, otherwise the terminated one |
| `D004` | Container cannot be created or started | Container waiting with `CreateContainerConfigError`, `CreateContainerError` or `RunContainerError` (the runtime cannot start the container's process), or waiting in `CrashLoopBackOff` whose last termination is `StartError` (the same failure between two attempts; takes precedence over `D001`) | No |
| `D005` | Pod cannot be scheduled | The Pod still `Pending`, with its `PodScheduled` condition `False` for reason `Unschedulable`, or `SchedulerError` when a scheduler plugin failed for it (a volume that cannot be bound); never `SchedulingGated` | No |
| `D006` | Probe failures | Warning event `Unhealthy` for a readiness, liveness or startup probe, happening again a settle window after it was first seen | No |
| `D007` | Volume cannot be attached or mounted | Warning event `FailedMount` or `FailedAttachVolume` | No |
| `D008` | Job failed | Warning event `BackoffLimitExceeded`, `DeadlineExceeded`, `PodFailurePolicy`, `MaxFailedIndexesExceeded` or `FailedIndexes` about the Job (every reason of a Job's `Failed` condition) | Yes, the terminated container of one failed Pod of the Job (the first by name) |
| `D009` | Pod evicted | The Pod in phase `Failed` with `status.reason` `Evicted` (its containers' `ephemeral-storage` limits, an `emptyDir` `sizeLimit`, or node pressure; the status message says which), failed in the last 10 minutes, unless the message starts with `Pod was rejected:` | No |
| `D010` | Pod sandbox cannot be created | Warning event `FailedCreatePodSandBox` about a Pod | No |
| `D011` | Workload cannot create Pods or Jobs | Warning event `FailedCreate` about a ReplicaSet, a Job, a StatefulSet, a DaemonSet or a CronJob (a quota, an admission webhook or Pod Security admission refused the Pod, or the CronJob's Job). About a CronJob itself (not a Job of a CronJob), it is an incident at once | No |
| `D012` | Volume claim cannot be provisioned | Warning event `ProvisioningFailed` about a PersistentVolumeClaim | No |
| `D013` | Autoscaler cannot get metrics | Warning event `FailedGetResourceMetric`, `FailedGetContainerResourceMetric`, `FailedGetPodsMetric`, `FailedGetObjectMetric`, `FailedGetExternalMetric`, `FailedComputeMetricsReplicas` or `FailedGetScale` about a HorizontalPodAutoscaler | No |
| `D014` | Rollout does not progress | A Deployment whose `Progressing` condition is `False` with reason `ProgressDeadlineExceeded` (exact case), that is not paused, and whose `status.observedGeneration` is at least `metadata.generation` (both present); a state, about the Deployment itself. Yields to an explained incident of the same Deployment (below). Off with `--no-rollouts` | No |

For `D005`, the request carries the unschedulable Pod's node selector, required affinity and
toleration keys, with no values but the fixed `kubernetes.io/os` and `kubernetes.io/arch` ones: on
Kubernetes 1.36.4 (kind, 2026-10-29) the scheduler's message names no key, value, taint or node
(`1 node(s) didn't match Pod's node affinity/selector`, `had untolerated taint(s)`). For `D006`,
and for `D001` since a failing liveness probe ends in `CrashLoopBackOff`, it carries each HTTP
probe's path without its query and fragment ([0017. Send the model resource requests, probe
handlers, image pull settings and an unschedulable Pod's scheduling keys, reduced and
redacted](../adr/0017-richer-context-for-the-model.md); the fields in [Data sent to the
provider](data-sent-to-the-provider.md)).

`D005` reads the Pod's condition rather than the scheduler's `FailedScheduling` events, which may
stop being updated for a Pod that keeps failing (hypothesis about client-go's `events.k8s.io`
broadcaster, to verify on a live cluster); the condition holds as long as the failure. A Pod
whose claim the `VolumeBinding` plugin could not bind in `PreBind` (a `ReadWriteMany` claim on a
`WaitForFirstConsumer` class that does not support it) stayed `Pending` for more than 25 minutes
with reason `SchedulerError`, while its `FailedScheduling` event repeated twice in 16 minutes (seen
on kind, Kubernetes 1.37, on 2026-10-28). A transient `SchedulerError` is absorbed by the settle
window (hypothesis, not reproduced). `D008` needs no watch on Jobs: the Job controller emits a Warning event on the Job, with the reason
of its `Failed` condition, when it marks the Job failed (`pkg/controller/job/job_controller.go` in
Kubernetes, checked on 2026-10-26). A container recovering after one out-of-memory kill, and the
kubelet's own `BackOff` events (the container state already says it), match nothing. Hypothesis
still to verify against live clusters: that the kubelet and scheduler reasons above are the ones
current releases emit (they are the long-standing ones).

**Grouping and fingerprint.** The incident key is the namespace, the top-level owner (found by
following owner references: Pod → ReplicaSet → Deployment, Pod → StatefulSet, Pod → DaemonSet,
Pod → Job → CronJob, or the Pod itself) and the detection id. kausa follows the controller owner
reference only through kinds it may `get` (ReplicaSet, Job). When the next owner is of another
kind, including a custom resource, that owner reference is the top-level owner. It is named in the
incident key and in `regarding` from the reference alone (apiVersion, kind, name, uid), without
reading the object. An event about an object other than a Pod (`D008`, `D011` to `D013`) needs
no Pod: its incident is about that object's top-level owner, found the same way from the object
itself (a ReplicaSet's Deployment, a Job's CronJob), and a StatefulSet, a DaemonSet, a
PersistentVolumeClaim or a HorizontalPodAutoscaler is its own top-level owner, named from the
event's `regarding`; no new permission is needed. A CronJob in an event's `regarding` (`D011`) is its own top-level owner: kausa reads nothing to
find it, as for a StatefulSet. A container matches at most one detection, the
most specific one: `D003` takes precedence over `D001` when the last termination is `OOMKilled`,
and `D004` when it is `StartError`, before grouping. No
container of a Pod in phase `Failed` is a crash loop: such a Pod runs no container again, and a
container that had restarted before the kubelet evicted the Pod or ended it at its
`activeDeadlineSeconds` stays terminated (`Error`, exit code 137) after a restart (seen on kind,
Kubernetes 1.37, on 2026-10-28).

`D009` reads the Pod's status rather than the kubelet's `Evicted` event, which is written once per
Pod (count 1) and so would never settle; the ReplicaSet's replacement Pods, evicted again every 15
to 20 seconds when the workload's own limit is the cause, are one incident. Evicted Pods are not
deleted, so an eviction is a signal only while its failure time (the later of the `Ready`
condition's last transition and the containers' finish times) is at most 10 minutes old, like an
event; a Pod with neither is no signal. Without that bound, every old eviction would become an
incident again each time kausa starts. The kubelet also writes `Evicted` when it refuses a new Pod
on a node under pressure (`Pod was rejected: The node had condition: [DiskPressure]`): a
consequence of the node's state, not of the workload, and no signal. `D009` reads no logs: the
evicted container's log is gone, and node pressure is not the application's fault. A node-pressure
eviction can hit many workloads at once, one incident each, which the call budget bounds. All of
this was seen on kind (Kubernetes 1.37) on 2026-10-28. In the kubelet, the eviction manager sets
phase `Failed`, reason `Evicted` and the message on the Pod it evicts (`evictPod` in
`pkg/kubelet/eviction/eviction_manager.go`; the reason and the message formats, among them the
`emptyDir` one that names the volume, in `pkg/kubelet/eviction/helpers.go`), and a Pod refused at
admission gets the same reason with its message prefixed `Pod was rejected: ` (`rejectPod` in
`pkg/kubelet/kubelet.go`); checked in Kubernetes v1.37.0 and on `master` on 2026-10-28. The
request carries each Pod's status reason and message, redacted, because only the message tells the
three causes apart ([0016. Send each Pod's status reason and message to the model, redacted and
bounded](../adr/0016-send-the-pod-status-reason-and-message.md)).

`D010` covers a Pod whose sandbox the container runtime cannot create: a `RuntimeClass` whose
handler the nodes do not have, or (hypothesis, not reproduced) a network plugin that cannot set up
the Pod's network. The Pod waits in `ContainerCreating`, which matches no state. The kubelet
retries about every 12 seconds and writes each failure as its own event (count 1, a new sandbox id
in each message), until its event aggregation folds them into one event with the prefix `(combined
from similar events): ` that it keeps updating; that event settles like `D006` and `D007`, grouped
by the Pod's owner, so the first incident comes about three minutes after the Pod was created.
The kubelet emits the event, with the note `Failed to create pod sandbox: <error>`, when creating
the sandbox fails in `SyncPod` (`pkg/kubelet/kuberuntime/kuberuntime_manager.go`; the reason is
defined in `pkg/kubelet/events/event.go`), and the prefix is the one client-go's event aggregator
adds (`staging/src/k8s.io/client-go/tools/record/events_cache.go`); checked in Kubernetes v1.37.0
and on `master` on 2026-10-28.
`D010` reads no logs: no container ran. The note names the runtime handler, and the 64-character
sandbox id in it is redacted (`R012`). Transient sandbox failures while a node starts should be
absorbed by the settle window (hypothesis, not reproduced), and a network plugin broken on a whole
node gives one incident per workload on it, which the call budget bounds. Seen on kind (Kubernetes
1.37) on 2026-10-28.

`D011` covers a workload whose controller cannot create its Pods because the API server refuses
them: a ResourceQuota (`exceeded quota: <name>, requested: …, used: …, limited: …`), Pod Security
admission (`violates PodSecurity "restricted:latest": …`) or an admission webhook. No Pod exists,
so the Warning `FailedCreate` event is about the controller: the ReplicaSet, Job and DaemonSet
controllers write `Error creating: <error>` through the shared Pod control
(`FailedCreatePodReason` and `RealPodControl` in `pkg/controller/controller_utils.go`), and the
StatefulSet controller writes `Create Pod <pod> in StatefulSet <set> failed error: <error>`
(`recordPodEvent` in `pkg/controller/statefulset/stateful_pod_control.go`), or, with the same
reason, `Create Claim <claim> for Pod <pod> in StatefulSet <set> failed error: <error>` when it
cannot create a claim of its `volumeClaimTemplates` (`recordClaimEvent`, same file; not reproduced,
hypothesis that it shows the same way); the quota's message is
in `staging/src/k8s.io/apiserver/pkg/admission/plugin/resourcequota/controller.go` and Pod
Security's in `staging/src/k8s.io/pod-security-admission/admission/admission.go`; checked in
Kubernetes v1.37.0 and on `master` on 2026-10-29. An event about a ReplicaSet is an incident about
its Deployment and one about a Job about its CronJob, through the lookups kausa already makes for
Pods. The first ten refusals are separate events, each naming the Pod the controller tried to
create (a generated name), until client-go's aggregation folds them into one event with the prefix
`(combined from similar events): ` (`defaultAggregateMaxEvents` 10 within 600 seconds in
`staging/src/k8s.io/client-go/tools/record/events_cache.go`), which settles like `D006` and
`D007`. On kind (Kubernetes 1.37, 2026-10-28), the Job controller kept updating its event, while
the ReplicaSet's stopped after 11 minutes and the DaemonSet's after 16, with the failure still
there (hypothesis: the controllers' rate-limited requeue backs off to minutes): the incident clears
10 minutes later and comes back with the next event, with the same fingerprint, so a repeat reuses
the cached explanation. A refusal that lasts less than the settle window, such as a StatefulSet
blocked for ten seconds until the quota freed, is no incident. A Deployment's own conditions
(`ReplicaFailure`) say the same; kausa watches Deployments for `D014` but does not keep that
condition, and its message is never sent (the event's note, redacted, is). `D011` reads no logs: no
container ran.

`D012` covers a PersistentVolumeClaim that cannot be provisioned: a StorageClass that does not
exist, or a request the external provisioner refuses. The PersistentVolume controller writes the
Warning `ProvisioningFailed` on the claim (`provisionClaim` in
`pkg/controller/volume/persistentvolume/pv_controller.go`, the reason in
`pkg/controller/volume/events/event.go`; Kubernetes v1.37.0 and `master`, checked on 2026-10-29),
and so do external provisioners built on `sig-storage-lib-external-provisioner`, with the note
`failed to provision volume with StorageClass "<name>": <error>` (`controller/controller.go`,
v11.0.1, the version of `local-path-provisioner` v0.0.34, which kind v0.33.0 builds; checked on
2026-10-29). The claim is the
incident's object, whether a Pod uses it or not. On kind (2026-10-28) the in-tree event for a
missing StorageClass kept being updated about every 15 seconds, while the external provisioner's
stopped after 16 minutes with the claim still `Pending`, so that incident clears and comes back
like `D011`'s. A provisioner that does not run at all writes only the Normal `ExternalProvisioning`
event, which kausa does not watch: the Pods that wait for the claim are `D005`. `D012` reads no
logs.

**`D012` and `D005`.** A Pod that mounts a claim that cannot be provisioned cannot be scheduled
either (`pod has unbound immediate PersistentVolumeClaims` with `Unschedulable`, or a
`SchedulerError` from the `VolumeBinding` plugin for a `WaitForFirstConsumer` class), so one cause
gives two incidents: `D005` about the workload and `D012` about the claim, each explained on its
own object, at the cost of two calls. kausa does not record which claims a Pod mounts, so it cannot
tell that the two are one failure, and the external provisioner's event goes quiet while the Pod's
condition holds, so folding one into the other would still give a second explanation later. The
`D005` explanation sees only the scheduler's message; the `D012` one sees the claim's events,
which name the cause.

`D013` covers a HorizontalPodAutoscaler that cannot read its metrics (no metrics API, a metric
that does not exist), compute a replica count from them, or read its target's scale (a target that
does not exist). The HorizontalPodAutoscaler controller writes each of these as a Warning event on
the autoscaler: the `FailedGet…Metric` reasons through `getUnableComputeReplicaCountCondition`,
which also sets `ScalingActive` to `False`; `FailedGetScale`, with `AbleToScale` `False`; and
`FailedComputeMetricsReplicas` when no metric gave a replica count
(`pkg/controller/podautoscaler/horizontal.go`; Kubernetes v1.37.0 and `master`, checked on
2026-10-29). The autoscaler is the incident's object, and all its reasons are one incident. On kind
without a metrics server (2026-10-28), `FailedGetResourceMetric` and `FailedGetScale` were updated
about every 15 seconds, while `FailedComputeMetricsReplicas` stopped after three minutes with the
failure still there; `FailedGetPodsMetric`, `FailedGetObjectMetric`, `FailedGetExternalMetric` and
`FailedGetContainerResourceMetric` come from the same code path but were not reproduced
(hypothesis: they repeat the same way). A failed rescale (`FailedRescale`, after the metrics were
read) is not `D013`. The Pods of the target are unaffected, so nothing else is raised. `D013`
reads no logs.

`D014` covers a Deployment whose rollout stopped progressing while nothing fails: a readiness
gate nobody sets, a `minReadySeconds` never reached, Pods held by their scheduling gates (`D005`
ignores `SchedulingGated`), probes that fail too seldom for `D006`. After
`spec.progressDeadlineSeconds` (600 by default) without progress, the deployment controller sets
the Deployment's `Progressing` condition to `False` with reason `ProgressDeadlineExceeded`
(`syncRolloutStatus` in `pkg/controller/deployment/progress.go`) and **emits no Event**, so only a
watch on Deployments sees it; progress or a new rollout sets the condition back to `True`, and an
unset deadline removes it. A paused Deployment gets `Unknown` with `DeploymentPaused`, except
after a timeout: `checkPausedConditions` (`pkg/controller/deployment/sync.go`) does not overwrite
`ProgressDeadlineExceeded`, so `D014` also checks `spec.paused`. A condition older than the last
edit of the Deployment does not count: `status.observedGeneration` must be at least
`metadata.generation`, both present. Kubernetes source read on 2026-10-29. The comparisons are
exact, and the record kausa keeps of a Deployment holds only what the rule needs ([Data sent to the
provider](data-sent-to-the-provider.md)). `D014` reads no logs.

**`D014` yields to an explained incident.** Pod rules already explain most stalled Deployments on
the same key (the top-level owner of a crash-looping or refused rollout is the Deployment), so
`D014` is decided after every other key of the same check, and a `D014` not yet emitted, with
another pending key of the same namespace, owner kind and owner name, is:

- **handled**, with no incident, when one of those keys was emitted, on this check or before (a
  `D001` or a `D011` that settles on the same check counts). A handled `D014` stays silent for the
  whole stall: while its signal lasts, and 330 seconds after;
- **deferred** when none of them was emitted: an event detection still fresh, or a state within
  its settle window or its 330 seconds of grace. Its own settle window keeps running, so it is
  emitted on the first check the others are gone, once.

The yield goes one way: a Pod rule or `D011` that starts after `D014` was emitted is still
explained. A Pod whose ReplicaSet lookup failed is grouped under the ReplicaSet, not the
Deployment, so it does not hold `D014` back, and both are explained. A signal that never settles
delays `D014` instead of silencing it: one Warning `Unhealthy` event (a `D006` signal for 10
minutes) until 10 minutes after it, a `D005` that clears within the window until 330 seconds after
it clears.

**No rule for StatefulSets or DaemonSets.** They have no progress deadline and no failure
condition: the alpha `Recreate` strategy of StatefulSets sets `Progressing` only to `True` in the
source read (hypothesis that it never sets `False`), and `OnDelete` and `partition` rollouts wait
by design. Their stalls are failing Pods (`D001` to `D010`, grouped under the StatefulSet or
DaemonSet) or refused Pods (`D011`); kausa has no permission on them.

The **fingerprint** hashes the detection id, the namespace, the owner's kind and name, and a text
that stays the same for the same failure: the example's message without the prefix
`(combined from similar events): ` of an aggregated event (so that the kubelet's first `D010`
events and the event that folds them have one fingerprint), with the names of the incident's
Pods (and, for `D011`, of the Pods the controller tried to create or the Job of a CronJob's
scheduled time, and, for `D008`, of the Pod a `PodFailurePolicy` note names, all of which start
with the top-level owner's name), UUIDs, Go durations (the kubelet's growing back-off: `40s`, `2m40s`, `5m0s`), hexadecimal runs
and other numbers normalized; for `D002`, whose message alternates between `ErrImagePull` and
`ImagePullBackOff`, the image reference instead; for `D001` and `D003`, whose example is the
container waiting in `CrashLoopBackOff` (with the back-off message) or terminated between restarts
(with none) depending on when kausa looks, the container's name, and so for a `D004` container
that cannot start, whose example alternates between `RunContainerError` (the runtime's message)
and `CrashLoopBackOff` (the back-off message) and is logged with the reason `RunContainerError`
either way; for `D014`, the text
`revision <n>`, `<n>` the Deployment's `deployment.kubernetes.io/revision` annotation when it
parses as an unsigned integer (the text is empty otherwise), hashed as is, not normalized, so each
rollout has its own fingerprint, stable across restarts. It is computed before redaction and never
sent to the model; it is in kausa's log, the names of its Events and Explanations and, with
notifications on, every notification ([Data sent in
notifications](data-sent-in-notifications.md#the-fingerprint)). It also keys the deduplication
cache.

### The coverage additions of 2026-11-02

Each change below was reproduced on kind (Kubernetes v1.36.4) on 2026-11-02, and each citation is
the Kubernetes source at the tag `v1.36.4`. No id was added, and no permission or request field.

**`D008`: every reason of a Job's `Failed` condition.** The Job controller's `recordJobFinished`
writes the reason and message of the Job's `Failed` condition as a Warning event on the Job
(`pkg/controller/job/job_controller.go:1666`), and the reason is one of five
(`pkg/apis/batch/types.go:622-633`): `BackoffLimitExceeded`, `DeadlineExceeded`,
`PodFailurePolicy` (a failed Pod matched a `FailJob` rule of the Job's `podFailurePolicy`,
`job_controller.go:1081`), and `MaxFailedIndexesExceeded` and `FailedIndexes` (an indexed Job with
`backoffLimitPerIndex`, `:1107` and `:1109`). On kind each came once (count 1), with the
condition's message: `Container <container> for pod <namespace>/<pod> failed with exit code <n>
matching FailJob rule at index <i>` (`pkg/controller/job/pod_failure_policy.go:57`; a rule on a Pod
condition writes `Pod <namespace>/<pod> has condition <type> matching <action> rule at index <i>`,
`:74`, not reproduced), or the fixed `Job has exceeded the specified maximal number of failed
indexes` and `Job has failed indexes`. The interim `FailureTarget` condition writes no event. The
failed Pods are kept (phase `Failed`, terminated `Error`, no restart), so the log of the first
failed Pod is read as for the other reasons. A `D008` incident gathers no Pod, so the container and
the Pod in a `PodFailurePolicy` note are no registered names: `R003` redacts them, and the
fingerprint replaces the Pod's generated name (`<job>-<suffix>`, or `<cronjob>-<n>-<suffix>` when
the owner is the Job's CronJob), so a rerun or the next scheduled time keeps the fingerprint.
Which indexes failed is only in the Job's status, which kausa does not read.

**`D011` about a CronJob.** The CronJob controller writes `FailedCreate` on the CronJob, `Error
creating job: <error>`, when the API server refuses the Job of a scheduled time
(`pkg/controller/cronjob/cronjob_controllerv2.go:640`). With a ResourceQuota `count/jobs.batch:
"0"` and a schedule of every minute, the first scheduled time gave one event whose series reached
count 14 in 41 seconds, the controller retrying the same Job name (`jobs.batch "<cronjob>-<n>" is
forbidden: exceeded quota: <quota>, requested: count/jobs.batch=1, …`), and each later scheduled
time gave a new event, count 1, naming a new Job. The CronJob's status stayed empty and no Job or
Pod existed. Under the event rule of the settle window this would never be an incident: the first
series ends before a 60-second window, each later event is a new object seen once, and an hourly
CronJob's next event comes after the last one is no longer fresh (10 minutes). So a `D011` event
whose `regarding` is the CronJob does not wait for the settle window, like a failed Job; it is
forgotten as before, once no fresh event remains. The exemption follows the event's object, not
the incident's key: a Job of a CronJob whose Pods are refused is keyed to the CronJob too (Job →
CronJob), but the Job controller keeps retrying its Pods while the Job is active, so its events
wait for the window as before, and a refusal that fixes itself (an admission webhook down for a
few seconds) is no incident. The CronJob is named from `regarding`, with no read and no new
permission, and the Job's numbered name counts as a generated name in the fingerprint, so
every scheduled time has the same fingerprint. The note names a Job and a quota, which `R003`
redacts (the qualified resource `jobs.batch "<name>"` and `exceeded quota: <name>`).

**`D002` and `ErrImageNeverPull`.** With `imagePullPolicy: Never` and an image the node cannot use,
the container waits with `ErrImageNeverPull` (`pkg/kubelet/images/types.go:38`) and the message
`Container image "<image>" is not present with pull policy of Never` (48 of 48 samples over 4
minutes, the Pod `Pending`, no restart). The kubelet returns the same reason when the image is
present but the Pod's credentials cannot use it (`pkg/kubelet/images/image_manager.go:150-159`), so
the rule does not say the image is absent. The fingerprint text is the image reference, as for the
other reasons of `D002`; the image in the message gets the same placeholder as the `image` field,
and the request carries `image_pull_policy` `Never`.

**`D004` and a container that cannot start.** When the runtime creates the container but cannot
start its process (an entrypoint that does not exist or is not executable), the kubelet writes the
Warning `Failed` event `Error: <runtime message>` and returns `RunContainerError`
(`pkg/kubelet/container/sync_result.go:87`,
`pkg/kubelet/kuberuntime/kuberuntime_container.go:292-296`). The waiting reason it keeps in its
cache overrides the terminated state (`pkg/kubelet/kubelet_pods.go:2731-2748`): the container
shows waiting `RunContainerError` with last termination `StartError` (exit code 128), and, while
it backs off, `CrashLoopBackOff` with the same last termination
(`pkg/kubelet/kuberuntime/kuberuntime_manager.go:1934-1961`); `state.terminated` never appeared.
Over 6 minutes the waiting reason was `RunContainerError` in 114 of 126 samples, and
`CrashLoopBackOff` first came about 2 minutes 46 seconds after the Pod was created. Before, `D001`
caught it late and only through the `CrashLoopBackOff` samples, and read the log of a container
that never ran. Now both states are `D004`: `CrashLoopBackOff` with last termination `StartError`
takes precedence over `D001`, as `OOMKilled` does for `D003`, so it is one incident, settled as a
state, with no log read. Its sample is logged with the reason `RunContainerError` whichever state
it was, and its fingerprint text is the container's name. The waiting message (redacted, and sent
as for any Pod before) can name the executable path the runtime could not start.

**Non-findings and hypotheses.**

- A container terminated with `StartError` after a restart, on the restart-count path of `D001`,
  stays `D001`: it never appeared on kind (the waiting reason overrides it).
- Why the kubelet reports `RunContainerError` rather than `CrashLoopBackOff` most of the time is a
  hypothesis, not traced.
- A Job whose name is long enough that the generated names of its Pods truncate it has Pods whose
  names do not start with `<job>-`, so the Pod in its `PodFailurePolicy` note is not replaced in the
  fingerprint text, and each rerun may get a fingerprint of its own (hypothesis from the
  generated-name rule, not reproduced).
- Lifecycle-hook failures (`FailedPostStartHook`, `FailedPreStopHook`) stay out of scope: their
  notes quote the hook's command, which [0008. Send only allowlisted, redacted data to one
  OpenAI-compatible endpoint](../adr/0008-data-leaving-the-cluster.md) keeps from the model.

## Redaction rules

Implemented in `src/redact.rs` (second implementation weekend). `R001` works on structure: only the
fields of [Data sent to the provider](data-sent-to-the-provider.md) are ever serialized. The others
work on every name and free text that goes into a request (event messages, the waiting, scheduling
and Pod status messages, the Pod status reason, log lines, probe paths, and node selector, affinity
and toleration keys). First, text from the cluster that already looks like one of kausa's
placeholders (`pod-1`, `deployment-2`) has its hyphen turned into an underscore, unless it is a real
name kausa registered, so that the answer cannot map it to a real name. Then the rules run in this
order: the secrets first (`R009`, `R014`, `R007`, `R008`, `R006`, `R010`, `R011`), so that a known
name inside a secret's key cannot break the key apart; then `R002`; then `R005`, `R004`, `R003`,
`R013` and `R012`. Each match is replaced by a placeholder, such as `<redacted:jwt>`, and counted;
the counts per rule are in the `request` and `explanation` log lines ([Command line and log
format](command-line.md)).

| Id | What | How |
|---|---|---|
| `R001` | Fields outside the allowlist | Never serialized. A resource quantity (a request or a limit) is kept only if it is at most 32 bytes and in the Kubernetes quantity grammar, otherwise sent as `null`; pull policies, probe handlers, scheduling operators and effects are mapped to closed sets |
| `R002` | Known names | The namespace, the owner, the incident's Pods, their containers, nodes and service accounts, and the registry host and path of images outside the public registries (`docker.io`, `ghcr.io`, `quay.io`, `registry.k8s.io`, `gcr.io`, `mcr.microsoft.com`, `public.ecr.aws`; an image with no registry is from Docker Hub), replaced by stable placeholders (`namespace-1`, `deployment-1`, `pod-1`, `container-1`, `node-1`, `serviceaccount-1`, `registry-1`, `image-1`; `object-1` for a custom resource's kind) wherever they appear as words, in any ASCII case: a hyphen, dot, slash, underscore or percent escape is a word boundary, so a Deployment's name is replaced inside its ReplicaSet's name and in `PAYMENTS_SERVICE_HOST` too. Mapped back in the answer in one pass, also when the model capitalizes them |
| `R003` | Names in Kubernetes messages | The quoted name after `secret`, `configmap`, `persistentvolumeclaim`, `pvc`, `serviceaccount`, `service`, `pod`, `container` or `StorageClass` (singular or plural, single or double quotes, every name of a list such as `containers "a", "b"`): `secret "<redacted:name>" not found`, `pods "<redacted:name>" is forbidden`; the quoted name after a resource qualified by its API group: `deployments.apps "<redacted:name>" not found`, `storageclass.storage.k8s.io "<redacted:name>" not found`; the quota's name in `exceeded quota: <name>`; the name in `Secret <namespace>/<name>` and `ConfigMap <namespace>/<name>`; the name in `system:serviceaccount:<namespace>:<name>`; the container and the Pod in the Job controller's `podFailurePolicy` notes, `Container <redacted:name> for pod <namespace>/<redacted:name> failed with exit code <n> matching <action> rule at index <i>` and `Pod <namespace>/<redacted:name> has condition <type> matching <action> rule at index <i>` (since 2026-11-02: a `D008` incident gathers no Pod, so neither name is registered). The names after `pod`, `container`, `StorageClass`, a qualified resource and in a list are a second pass over the output of the first (the six kinds before them, one quoted name each), and contain no whitespace, so a quote left open cannot hide a name the first pass catches |
| `R004` | IP addresses | IPv4 addresses, and IPv6 addresses that parse as such once the colons around them are dropped (so `10:05:00` and `Foo::Bar` stay); the port after them is kept |
| `R005` | Email addresses | `local@domain.tld` |
| `R006` | Credentials in URLs | The user information of a URL, empty user and passwords with `@` or `/` included: `postgres://<redacted:userinfo>@db:5432`, `redis://<redacted:userinfo>@redis:6379`. The host is left to `R013` |
| `R007` | Authorization values and cookies | The value of `Authorization:`, `Proxy-Authorization:`, `Cookie:` and `Set-Cookie:` (quoted, escaped or to the end of the line, any scheme), and `Bearer` or `Basic` followed by eight or more token characters |
| `R008` | JSON Web Tokens | Three base64url segments separated by dots, the first one starting with `eyJ` (a JSON header) |
| `R009` | Private keys and certificates | PEM blocks, `-----BEGIN …-----` to `-----END …-----`, any case |
| `R010` | Values of secret-named keys | The value in `key=value`, `key: value`, JSON `"key": "value"` (escaped JSON inside JSON too), `key%3Dvalue`, `--key value` and `<key>value</key>`, where the key ends in `password`, `passwd`, `passphrase`, `pass`, `pwd`, `secret`, `secret_key`, `secret_key_base`, `signing_key`, `encryption_key`, `master_key`, `auth_key`, `token`, `api_key`, `access_key`, `private_key`, `credential(s)`, `auth`, `signature`, `sig`, `session`, `session_id` or `sid`, in any case (`DB_PASSWORD=`, `client_secret:`, `MYSQL_PWD=`). A quoted value is redacted whole, spaces included |
| `R011` | Known token formats | GitHub (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` and 36 characters; `github_pat_`), GitLab (`glpat-`, `gldt-`, `glrt-`, `glrtr-`, `glcbt-`, `glptt-`, `glft-`, `glimt-`, `glagent-`, `glwt-`, `glsoat-`, `glffct-`, `gloas-` and 20 or more characters; prefixes from [GitLab token overview](https://docs.gitlab.com/security/tokens/)), npm (`npm_` and 36 or more letters and digits: a Base62 checksum of six characters after 178 bits of entropy, from [Announcing npm's new access token format](https://github.blog/security/announcing-npms-new-access-token-format)), PyPI (`pypi-` and 85 or more characters, from [Secret reporting API](https://docs.pypi.org/api/secrets/)), Docker (`dckr_pat_`, `dckr_oat_` and 10 or more characters; prefixes from [AI Governance API](https://docs.docker.com/reference/api/ai-governance/latest/)), Slack (`xoxa-`, `xoxb-`, `xoxp-`, `xoxo-`, `xoxs-`, `xoxr-`), AWS access key ids (`AKIA` or `ASIA` and 16 characters), Stripe (`sk_live_`, `sk_test_`, `rk_live_`, `rk_test_`), OpenAI and Anthropic style keys (`sk-`, `sk-proj-`, `sk-ant-` and 20 or more characters), Google API keys (`AIza` and 35 characters). Hypothesis to verify against each issuer's documentation: that the formats without a source are the current ones, and the lengths of GitLab's and Docker's tokens, which their documentation does not give; `R012` catches most long tokens anyway |
| `R012` | High-entropy strings | Runs of 32 or more characters from `[A-Za-z0-9+/_-]` and percent escapes (with `=` padding) that contain a letter and a digit and have a Shannon entropy of at least 4 bits per character, or 3 for a hexadecimal run (whose ceiling is 4). A run with two or more slashes (a path or URL) is judged segment by segment, with segments of 20 or more characters |
| `R013` | Hostnames | Lowercase dotted names ending in a common top-level domain or an internal suffix (`.com`, `.net`, `.org`, `.io`, `.dev`, `.local`, `.lan`, `.internal`, `.corp`, `.svc`, `.svc.cluster.local`, `.home.arpa` and others); the host of any URL (`http://api.shop/`), whatever its suffix; and `name.other:port` (a Service in another namespace), unless its last label is a file extension (`main.rs:10`). The public registries and a few public hosts (`github.com`, `kubernetes.io`, `k8s.io`, the `example` domains) stay. Replaced by `host-1`, `host-2`…, never mapped back. Lowercase only, so Java packages and class names (`com.example.App`) stay |
| `R014` | Query and fragment of URLs | Whatever follows the first `?` or `#` of a URL with a scheme (`http://`, `https://`, `postgres://`…), up to a quote, an angle bracket, a backtick, a backslash or whitespace: `http://<redacted:ip>:8080/ready?<redacted:query>#<redacted:fragment>`. The scheme, host (left to `R004` and `R013`) and path stay, and so does punctuation that ends a sentence after the URL. Also the query and fragment of an HTTP request line without a scheme: an uppercase method (`GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS` or `TRACE`; not `CONNECT`, whose target is a host and port), spaces or tabs, and a target starting with `/`: `GET /orders?<redacted:query> HTTP/1.1`, `"POST /login?<redacted:query>#<redacted:fragment>"`. A lowercase method (`get /x?y=1`), a lone `?` or `#` (`GET /?`) and a path without a method stay. Runs right after `R009`, so a token in a query is counted here, not by `R008` or `R010` |

Rules are tested with a corpus of invented lines that must be redacted and lines that must stay
(ordinary log lines with numbers, timestamps, versions, durations, paths and public URLs), so that
a change in a rule shows its effect on both sides. Fixtures use only fake values: `example.com`,
the documentation ranges `192.0.2.0/24` and `2001:db8::/32`, and token strings built to match a
format, never ones that were issued.

## On the model's answer

Since v0.5 ([0022. Pass the model's answer through the pattern redaction rules before it is
written anywhere, and redact the query of an HTTP request line](../adr/0022-redact-the-answer.md)),
the model's answer passes the same pattern rules before it is logged, stored or written: after
`prompt::validate` has checked it against the schema and cleaned it, and before the names are
mapped back, `summary`, `cause` and each step go through `Redactor::answer` with the incident's own
redactor. `confidence` is a closed set and is not touched. The rules run in the order of the
request, `R009`, `R014`, `R007`, `R008`, `R006`, `R010`, `R011`, `R005`, `R004` (IPv4, then IPv6),
`R003`, `R013` and `R012`, the twelve ids of `ANSWER_RULES` in `src/redact.rs`, with the markers
the request uses. Two steps of the request pass are skipped:

- the neutralization of text that looks like a placeholder, which would turn the model's
  `deployment-1` into `deployment_1` and stop it being mapped back;
- `R002`, which would replace ordinary words that match a real name in any case ("the API server"
  beside a Deployment named `api`) and the public image references the request sends as written.
  `R001` is structural and does not apply to text.

The details that differ:

- `R013` uses one allowlist for both sides (the public registries and hosts above) and the
  incident's own host table: a host the request replaced keeps its `host-N`, a new one gets the
  next number, and `host-N` is never mapped back.
- Three details exist only on the answer, so that an answer that echoes kausa's own output passes
  unchanged and uncounted: kausa's own `<redacted:token>` and `<redacted:secret>` are not read as
  `R010` elements (`<token>value`), so `<redacted:token> was rejected` keeps its words; `R003` treats a placeholder whose
  first letter the model capitalized (`pods "Pod-1"`) and a quoted `<redacted:name>` as kausa's
  own; and a match counts only when its replacement differs from the text it matched.
- `R007` has no exception: after an echoed `Authorization: <redacted:authorization>`, the rest of
  the field is redacted too, so `Authorization: <redacted:authorization> was rejected` is written
  as `Authorization: <redacted:authorization>` (counted once).

The values replaced on the answer are counted apart from the request's: the `answer_redactions`
field of the `explanation` log line and `kausa_answer_redactions_total{rule}` ([Command line and log
format](command-line.md)). A new pattern rule joins `ANSWER_RULES` ([Add a detection or redaction
rule](../how-to/add-a-rule.md)); a test checks that the list matches the ids the request pass
counts, minus `R002`.

## Limits

- Redaction has false negatives: a secret in a format no rule knows, split across lines or
  encoded, still leaves. A local model is the answer for clusters where that is unacceptable
  ([Security policy](../../SECURITY.md)).
- Pseudonyms cover the names kausa knows (the incident's own objects) and the message formats
  above; the name of another workload, Pod or node that appears only inside a log line or an event
  message is caught by `R013` if it looks like a hostname, and otherwise is sent. So is an
  uppercase hostname, or a bare `service.namespace` without a port or a scheme.
- Probe paths and node selector, affinity and toleration keys are written by the workload's
  author, like a volume name. A customer name in a path, in a key without a prefix, under a public
  host's or registry's domain (`example.com`, `github.com`, `gcr.io`), under a suffix `R013` does
  not list (`karpenter.sh`) or under a subdomain of `kubernetes.io` or `k8s.io` is sent. A cloud
  provider's key (`cloud.google.com/…`, `eks.amazonaws.com/…`) becomes `host-N/…` (`R013`), never
  mapped back in the answer.
- `R014` covers URLs with a scheme and HTTP request lines. A query in a key-value or JSON form
  (`method=GET path=/x?y`, `"path":"/x?y"`, `uri=/x?y`) is not a request line and is sent.
- The same gaps hold on the model's answer, which an injected log line or event message can shape:
  commands (`curl … | sh`, `kubectl delete …`) are not redacted; nor are URLs and images on the
  allowlisted hosts and registries, path included (`github.com/<anyone>/…`, `example.com/…`), an
  uppercase host, a bare host under a suffix `R013` does not list (`name.test/x`), or values split,
  encoded or written in fullwidth characters. A misleading cause or a demanded confidence is not a
  value and stays.
- On the answer, redaction also costs readability: `0.0.0.0`, `127.0.0.1`, `::1` and versions
  shaped like addresses become `<redacted:ip>`; prose after `Authorization:` or `Cookie:` is lost
  to the end of the field; a word after `token:`, `auth:` or `password:` is replaced; "Basic
  authentication" becomes `Basic <redacted:authorization>`; documentation anchors lose their
  fragment; a quoted public image digest becomes `<redacted:secret>`; and links to public sites
  outside the allowlist (`docs.docker.com`) become `https://host-N/…`.
- A kubelet `Unhealthy` note for a probe's connection error quotes the probe URL; its query and
  fragment are redacted (`R014`), its path is not, and a note may name a port by its name
  (hypothesis, not reproduced), which the request's `port` never does; both are sent as event text
  ([0017. Send the model resource requests, probe handlers, image pull settings and
  an unschedulable Pod's scheduling keys, reduced and
  redacted](../adr/0017-richer-context-for-the-model.md)).
- Detection covers common workload failures, not every problem: a misbehaving application whose
  Pods stay `Running` and `Ready` produces no incident.
- `D014` does not see a Deployment with no progress deadline, a paused one (unless it was paused
  after the timeout), a stall shorter than the deadline plus the settle window and a check (about
  11 minutes with the defaults; lower the Deployment's `progressDeadlineSeconds` to hear sooner),
  or a `ReplicaSetCreateError` (set at once on any create error, transient ones included; the
  refusal is `D011`). StatefulSets and DaemonSets that wait on Pods that neither fail nor are
  refused raise nothing. With `--no-rollouts`, `D014` is off.
- A signal of the same Deployment that never settles delays `D014` until it is gone. Whoever can
  create Warning Events about the Deployment's Pods (or, through `D011`, its ReplicaSets) in its
  namespace, or make a Pod's probe fail, can delay it for as long as they keep producing fresh,
  distinct events; it comes 10 minutes after they stop. They cannot silence it for good: only an
  explained incident of the same Deployment does.
