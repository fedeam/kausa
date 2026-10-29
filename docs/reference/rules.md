# Detection and redaction rules

The deterministic rules kausa applies before any language model call: detection rules decide what
is an incident and how failures are grouped, and redaction rules decide what never leaves the
cluster.

Status as of 2026-10-29: the **detection rules, grouping and fingerprint are implemented** (first
implementation weekend, `src/detect.rs`, `src/incident.rs`, `src/fingerprint.rs`), and so are the
redaction rules and the log reading of the "Logs read" column (second weekend); `D009` and `D010`
and the changes to `D001` and `D005` came on 2026-10-28, and `D011` to `D013` and the new cases of
`R003` on 2026-10-29, all from failures reproduced in kind. Ids, triggers and
defaults may change until v0.1 is released; after that an id is never reused. How the rules fit in
the pipeline: [Architecture](../explanation/architecture.md). Why redaction works this way: [0008.
Send only allowlisted, redacted data to one OpenAI-compatible
endpoint](../adr/0008-data-leaving-the-cluster.md).

## Detection rules

A detection matches a signal: a **state** read from a Pod at every check (a container state, the
Pod's scheduling condition, or its eviction), or a Warning **event** about a Pod, a Job, a workload
controller (a ReplicaSet, a StatefulSet or a DaemonSet), a PersistentVolumeClaim or a
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
- **A failed Job** is final and is an incident at once.

Pods of the same top-level owner with the same detection form one incident, logged once while it
lasts and again only if it clears and comes back. The incident lists every Pod seen failing while
it was pending, so replicas whose crash loops are out of step all count.

| Id | Failure | Signal | Logs read |
|---|---|---|---|
| `D001` | Crash loop | Container waiting with reason `CrashLoopBackOff`, or terminated with any reason but `Completed` after at least one restart (between restarts the kubelet reports it as terminated for much of each back-off), in a Pod that is not in phase `Failed` | Yes: the previous instance while the container runs or waits again, otherwise the terminated one |
| `D002` | Image cannot be pulled | Container waiting with `ErrImagePull`, `ImagePullBackOff` or `InvalidImageName` | No |
| `D003` | Out of memory | Container waiting in `CrashLoopBackOff` whose last termination is `OOMKilled`, or terminated with `OOMKilled` and not restarted | Yes, previous container if it restarted, otherwise the terminated one |
| `D004` | Container cannot be created | Container waiting with `CreateContainerConfigError` or `CreateContainerError` | No |
| `D005` | Pod cannot be scheduled | The Pod still `Pending`, with its `PodScheduled` condition `False` for reason `Unschedulable`, or `SchedulerError` when a scheduler plugin failed for it (a volume that cannot be bound); never `SchedulingGated` | No |
| `D006` | Probe failures | Warning event `Unhealthy` for a readiness, liveness or startup probe, happening again a settle window after it was first seen | No |
| `D007` | Volume cannot be attached or mounted | Warning event `FailedMount` or `FailedAttachVolume` | No |
| `D008` | Job failed | Warning event `BackoffLimitExceeded` or `DeadlineExceeded` about the Job | Yes, the terminated container of one failed Pod of the Job (the first by name) |
| `D009` | Pod evicted | The Pod in phase `Failed` with `status.reason` `Evicted` (its containers' `ephemeral-storage` limits, an `emptyDir` `sizeLimit`, or node pressure; the status message says which), failed in the last 10 minutes, unless the message starts with `Pod was rejected:` | No |
| `D010` | Pod sandbox cannot be created | Warning event `FailedCreatePodSandBox` about a Pod | No |
| `D011` | Workload cannot create Pods | Warning event `FailedCreate` about a ReplicaSet, a Job, a StatefulSet or a DaemonSet (a quota, an admission webhook or Pod Security admission refused the Pod) | No |
| `D012` | Volume claim cannot be provisioned | Warning event `ProvisioningFailed` about a PersistentVolumeClaim | No |
| `D013` | Autoscaler cannot get metrics | Warning event `FailedGetResourceMetric`, `FailedGetContainerResourceMetric`, `FailedGetPodsMetric`, `FailedGetObjectMetric`, `FailedGetExternalMetric`, `FailedComputeMetricsReplicas` or `FailedGetScale` about a HorizontalPodAutoscaler | No |

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
event's `regarding`; no new permission is needed. A container matches at most one detection, the
most specific one: `D003` takes precedence over `D001` when the last termination is `OOMKilled`,
before grouping. No
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
(`ReplicaFailure`) say the same, but reading them needs a new permission. `D011` reads no logs: no
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

**Which Events count.** A Warning Event counts only when its own `metadata.namespace` is non-empty
and equal to `regarding.namespace`, compared exactly (case-sensitive, no trimming). Events about
cluster-scoped objects (a Node, whose `regarding` has no namespace) and Events about an object in
another namespace are ignored: they match no detection and join no incident.

The **fingerprint** hashes the detection id, the namespace, the owner's kind and name, and a text
that stays the same for the same failure: the example's message without the prefix
`(combined from similar events): ` of an aggregated event (so that the kubelet's first `D010`
events and the event that folds them have one fingerprint), with the names of the incident's
Pods (and, for `D011`, of the Pods the controller tried to create, which start with the top-level
owner's name), UUIDs, Go durations (the kubelet's growing back-off: `40s`, `2m40s`, `5m0s`), hexadecimal runs
and other numbers normalized; for `D002`, whose message alternates between `ErrImagePull` and
`ImagePullBackOff`, the image reference instead; for `D001` and `D003`, whose example is the
container waiting in `CrashLoopBackOff` (with the back-off message) or terminated between restarts
(with none) depending on when kausa looks, the container's name. It is computed before
redaction and never sent to the model; it is in kausa's log, the names of its Events and Explanations and, with
notifications on, every notification ([Data sent in
notifications](data-sent-in-notifications.md#the-fingerprint)). It also keys the deduplication
cache.

## Redaction rules

Implemented in `src/redact.rs` (second implementation weekend). `R001` works on structure: only
the fields of [Data sent to the provider](data-sent-to-the-provider.md) are ever serialized. The
others work on every name and free text that goes into a request (event messages, the waiting,
scheduling and Pod status messages, the Pod status reason, log lines, probe paths, and node
selector, affinity and toleration keys). First, text from the cluster that already looks like one of
kausa's placeholders (`pod-1`, `deployment-2`) has its hyphen turned into an underscore, unless it
is a real name kausa registered, so that the answer cannot map it to a real name. Then the rules
run in this order: the secrets first (`R009`, `R007`, `R008`, `R006`, `R010`, `R011`), so that a
known name inside a secret's key cannot break the key apart; then `R002`; then `R005`, `R004`,
`R003`, `R013` and `R012`. Each match is replaced by a placeholder, such as `<redacted:jwt>`, and
counted; the counts per rule are in the `request` and `explanation` log lines ([Command line and
log format](command-line.md)).

| Id | What | How |
|---|---|---|
| `R001` | Fields outside the allowlist | Never serialized. A resource quantity (a request or a limit) is kept only if it is at most 32 bytes and in the Kubernetes quantity grammar, otherwise sent as `null`; pull policies, probe handlers, scheduling operators and effects are mapped to closed sets |
| `R002` | Known names | The namespace, the owner, the incident's Pods, their containers, nodes and service accounts, and the registry host and path of images outside the public registries (`docker.io`, `ghcr.io`, `quay.io`, `registry.k8s.io`, `gcr.io`, `mcr.microsoft.com`, `public.ecr.aws`; an image with no registry is from Docker Hub), replaced by stable placeholders (`namespace-1`, `deployment-1`, `pod-1`, `container-1`, `node-1`, `serviceaccount-1`, `registry-1`, `image-1`; `object-1` for a custom resource's kind) wherever they appear as words, in any ASCII case: a hyphen, dot, slash, underscore or percent escape is a word boundary, so a Deployment's name is replaced inside its ReplicaSet's name and in `PAYMENTS_SERVICE_HOST` too. Mapped back in the answer in one pass, also when the model capitalizes them |
| `R003` | Names in Kubernetes messages | The quoted name after `secret`, `configmap`, `persistentvolumeclaim`, `pvc`, `serviceaccount`, `service`, `pod`, `container` or `StorageClass` (singular or plural, single or double quotes, every name of a list such as `containers "a", "b"`): `secret "<redacted:name>" not found`, `pods "<redacted:name>" is forbidden`; the quoted name after a resource qualified by its API group: `deployments.apps "<redacted:name>" not found`, `storageclass.storage.k8s.io "<redacted:name>" not found`; the quota's name in `exceeded quota: <name>`; the name in `Secret <namespace>/<name>` and `ConfigMap <namespace>/<name>`; the name in `system:serviceaccount:<namespace>:<name>`. The names after `pod`, `container`, `StorageClass`, a qualified resource and in a list are a second pass over the output of the first (the six kinds before them, one quoted name each), and contain no whitespace, so a quote left open cannot hide a name the first pass catches |
| `R004` | IP addresses | IPv4 addresses, and IPv6 addresses that parse as such once the colons around them are dropped (so `10:05:00` and `Foo::Bar` stay); the port after them is kept |
| `R005` | Email addresses | `local@domain.tld` |
| `R006` | Credentials in URLs | The user information of a URL, empty user and passwords with `@` or `/` included: `postgres://<redacted:userinfo>@db:5432`, `redis://<redacted:userinfo>@redis:6379`. The host is left to `R013` |
| `R007` | Authorization values and cookies | The value of `Authorization:`, `Proxy-Authorization:`, `Cookie:` and `Set-Cookie:` (quoted, escaped or to the end of the line, any scheme), and `Bearer` or `Basic` followed by eight or more token characters |
| `R008` | JSON Web Tokens | Three base64url segments separated by dots, the first one starting with `eyJ` (a JSON header) |
| `R009` | Private keys and certificates | PEM blocks, `-----BEGIN …-----` to `-----END …-----`, any case |
| `R010` | Values of secret-named keys | The value in `key=value`, `key: value`, JSON `"key": "value"` (escaped JSON inside JSON too), `key%3Dvalue`, `--key value` and `<key>value</key>`, where the key ends in `password`, `passwd`, `passphrase`, `pass`, `pwd`, `secret`, `secret_key`, `secret_key_base`, `signing_key`, `encryption_key`, `master_key`, `auth_key`, `token`, `api_key`, `access_key`, `private_key`, `credential(s)`, `auth`, `signature`, `sig`, `session`, `session_id` or `sid`, in any case (`DB_PASSWORD=`, `client_secret:`, `MYSQL_PWD=`). A quoted value is redacted whole, spaces included |
| `R011` | Known token formats | GitHub (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` and 36 characters; `github_pat_`), GitLab (`glpat-`), Slack (`xoxa-`, `xoxb-`, `xoxp-`, `xoxo-`, `xoxs-`, `xoxr-`), AWS access key ids (`AKIA` or `ASIA` and 16 characters), Stripe (`sk_live_`, `sk_test_`, `rk_live_`, `rk_test_`), OpenAI and Anthropic style keys (`sk-`, `sk-proj-`, `sk-ant-` and 20 or more characters), Google API keys (`AIza` and 35 characters). Hypothesis to verify against each issuer's documentation: that these are the current formats; `R012` catches most long tokens anyway |
| `R012` | High-entropy strings | Runs of 32 or more characters from `[A-Za-z0-9+/_-]` and percent escapes (with `=` padding) that contain a letter and a digit and have a Shannon entropy of at least 4 bits per character, or 3 for a hexadecimal run (whose ceiling is 4). A run with two or more slashes (a path or URL) is judged segment by segment, with segments of 20 or more characters |
| `R013` | Hostnames | Lowercase dotted names ending in a common top-level domain or an internal suffix (`.com`, `.net`, `.org`, `.io`, `.dev`, `.local`, `.lan`, `.internal`, `.corp`, `.svc`, `.svc.cluster.local`, `.home.arpa` and others); the host of any URL (`http://api.shop/`), whatever its suffix; and `name.other:port` (a Service in another namespace), unless its last label is a file extension (`main.rs:10`). The public registries and a few public hosts (`github.com`, `kubernetes.io`, `k8s.io`, the `example` domains) stay. Replaced by `host-1`, `host-2`…, never mapped back. Lowercase only, so Java packages and class names (`com.example.App`) stay |

Rules are tested with a corpus of invented lines that must be redacted and lines that must stay
(ordinary log lines with numbers, timestamps, versions, durations, paths and public URLs), so that
a change in a rule shows its effect on both sides. Fixtures use only fake values: `example.com`,
the documentation ranges `192.0.2.0/24` and `2001:db8::/32`, and token strings built to match a
format, never ones that were issued.

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
- A kubelet `Unhealthy` note for a probe's connection error quotes the probe URL with its query
  and fragment, redacted by patterns only, and a note may name a port by its name (hypothesis, not
  reproduced), which the request's `port` never does; both are sent as event text ([0017. Send the model resource requests, probe handlers, image pull settings and
  an unschedulable Pod's scheduling keys, reduced and
  redacted](../adr/0017-richer-context-for-the-model.md)).
- Detection covers common workload failures, not every problem: a misbehaving application whose
  Pods stay `Running` and `Ready` produces no incident.
