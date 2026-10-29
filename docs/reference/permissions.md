# Permissions

The Kubernetes API permissions kausa's chart grants, and the ones it never asks for.

Status as of 2026-10-29: the chart in `charts/kausa` grants this table, `pods/log` only when
kausa explains incidents (an endpoint or dry-run) with log collection on, and the Event writes
only with an endpoint, outside dry-run, unless they are turned off, and the `explanations` row only
while kausa writes history (`history.enabled`, an endpoint, no `dryRun`; since v0.3.0), and the
`deployments` row while Deployments are watched for stalled rollouts (`rollouts`, on by default;
added for v0.4 on 2026-10-29), and, only with `replicaCount: 2`, `get` and `patch` on one Lease in
kausa's own namespace ([In kausa's own namespace, with two
replicas](#in-kausas-own-namespace-with-two-replicas); added for v0.4 on 2026-10-29) ([Helm
chart](helm-chart.md)). `rbac.scope` picks where: in every namespace (`cluster`), or in the listed
namespaces only (`namespaced`, since v0.3.0; see [Where the permissions
apply](#where-the-permissions-apply)). Why it is this list: [0009. Run with read-only permissions
that never include Secrets](../adr/0009-least-privilege-rbac.md); why there are two scopes: [0013.
Offer a namespaced install mode: one watch pair and one Role per listed namespace, and nothing
cluster-wide](../adr/0013-namespaced-install-mode.md); why kausa reads Deployments: [0018. Explain a
Deployment past its progress deadline (`D014`) from a Deployment watch that is on by default, and
add nothing for StatefulSets or DaemonSets](../adr/0018-stuck-rollouts.md); why a second replica
needs a Lease: [0019. Offer an optional standby replica that takes over through a chart-created
Lease, elected by kausa's own code](../adr/0019-high-availability.md). The kind test (`tests/e2e/run.sh`) checks
the installed role against this table with `kubectl auth can-i`, both what it allows and a list
of what it must refuse, in each scope; both scopes passed on 2026-10-28 on the maintainer's
machine. As of 2026-10-26,
`kausa run` uses every row: the Pod and event watches, the metadata `get` of ReplicaSets and Jobs,
`pods/log` for the detections that read logs, and `create` and `patch` on Events for its
explanations (with `--endpoint`, unless `--no-events`). On 2026-10-26 it
ran against a live cluster through a `kubectl proxy` that let only these reads through, plus
`POST` and `PATCH` on Events in one test namespace, and the proxy refused none of its requests ([Try kausa against a cluster with a local
model](../how-to/try-with-a-local-model.md)).

Notifications (`--notify-url-file`, the chart's `notifications.enabled`; since v0.3.0)
add no row and change no row, in either scope: a webhook `POST` is not a call to the Kubernetes
API, and the kubelet, not kausa, reads the Secret that holds the webhook URL ([0015. Post each new
explanation to one optional webhook, as JSON, Slack or Telegram](../adr/0015-notifications.md)).
The kind test checks that the `kubectl auth can-i` lists stay the same with notifications on.

## Granted

| API group | Resource | Verbs | Why |
|---|---|---|---|
| `""` (core) | `pods` | `get`, `list`, `watch` | Container states, restart counts, phase, owner references |
| `""` (core) | `pods/log` | `get` | The last lines of the failed container (the previous instance if it restarted) for crash-type and Job detections. Only while log collection is on |
| `events.k8s.io` | `events` | `list`, `watch` | Warning events about Pods, Jobs, ReplicaSets, StatefulSets, DaemonSets, PersistentVolumeClaims and HorizontalPodAutoscalers (the objects themselves are never read) |
| `events.k8s.io` | `events` | `create`, `patch` | Write the explanation, and update its series count when the incident repeats |
| `apps` | `replicasets` | `get` | Follow a ReplicaSet's owner reference to its Deployment, for a Pod or for an event about the ReplicaSet (`D011`) |
| `apps` | `deployments` | `list`, `watch` | The Deployment watch: a Deployment's `Progressing` condition, generation, revision and replica counts, for a rollout past its progress deadline (`D014`). Each Deployment is reduced at once to those fields, and kept only while it matches. Only while `rollouts` is on (the default); `rollouts: false` removes the row and passes `--no-rollouts`. No `get` |
| `batch` | `jobs` | `get` | Follow a Job's owner reference to its CronJob, for a Pod or for an event about the Job (`D008`, `D011`) |
| `kausa.fedeam.com` | `explanations` | `list`, `create`, `patch`, `delete` | Only while kausa writes history. `list`: read one object by name (a `metadata.name` field selector) before a write, and the sweep; `create`: an incident's first explanation; `patch`: append to its history (`status` and `resourceVersion` only; `spec` is immutable); `delete`: retention by age and by count ([Explanation resource](explanation-resource.md)) |

The `explanations` rule comes last in the rules and only with the history on; why kausa may write
its own resource: [0014. Keep each incident's full explanations in a namespaced Explanation
resource, off by default](../adr/0014-explanation-history.md).

## Where the permissions apply

The chart's `rbac.scope` is required and has no default: `helm install` and `helm upgrade` fail
without it. It changes where the rows above apply, never which rows there are:

| Rule | `rbac.scope: cluster` | `rbac.scope: namespaced` |
|---|---|---|
| `pods`: `get`, `list`, `watch` | every namespace | the listed namespaces only |
| `pods/log`: `get` (logs on) | every namespace | the listed namespaces only |
| `events.k8s.io` `events`: `list`, `watch` | every namespace | the listed namespaces only |
| `events.k8s.io` `events`: `create`, `patch` (Event writes on) | every namespace | the listed namespaces only |
| `apps` `replicasets`: `get` | every namespace | the listed namespaces only |
| `apps` `deployments`: `list`, `watch` (`rollouts` on, the default) | every namespace | the listed namespaces only |
| `batch` `jobs`: `get` | every namespace | the listed namespaces only |
| `kausa.fedeam.com` `explanations`: `list`, `create`, `patch`, `delete` (history on) | every namespace | the listed namespaces only |
| ClusterRole and ClusterRoleBinding | created | not created |

- **`cluster`**: one ClusterRole and one ClusterRoleBinding, as in v0.1, and kausa watches every
  namespace.
- **`namespaced`**: one Role and one RoleBinding in each namespace of `rbac.namespaces`, named
  with the release's full name and bound to kausa's service account in the release namespace;
  kausa is started with one `--namespace` per listed namespace, so the watch and the grant come
  from the same list. kausa is granted nothing outside the list: no Pod, log, Event, ReplicaSet,
  Deployment or Job in `kube-system`, in another tenant's namespace, or in its own namespace unless that is
  listed, and no list or watch across all namespaces. kausa needs nothing in its own namespace:
  the kubelet mounts its token and the API key.
- **Nothing on `namespaces`**, in either scope: kausa never checks whether a namespace exists.
- **`/version`**, which kausa reads once at start, needs no grant from the chart. In namespaced
  scope the chart grants nothing cluster-wide, and kausa still started and became ready in the
  kind test on 2026-10-28 (kind's default RBAC; that the default `system:public-info-viewer`
  ClusterRole is what allows it is a hypothesis, not checked).
- **Event writes, narrowed:** in namespaced scope kausa can create and patch Events in the listed
  namespaces only. Inside a listed namespace, RBAC still cannot limit them to kausa's own Events
  (see the [Security policy](../../SECURITY.md)).

## In kausa's own namespace, with two replicas

With `replicaCount: 2` (since v0.4; off by default), the chart renders
`templates/leader-election.yaml`: a Lease `<full name>-leader` in the release namespace, with the
chart's labels and `spec: {}`, a Role of the same name with one rule, and a RoleBinding of the same
name to kausa's service account. kausa runs with `--leader-lease <release namespace>/<full
name>-leader`, and only the replica that holds the Lease watches, explains and writes:

| API group | Resource | Resource names | Verbs | Why |
|---|---|---|---|---|
| `coordination.k8s.io` | `leases` | `<full name>-leader` | `get`, `patch` | `get`: read the holder and the `resourceVersion`, every 5 s; `patch`: a JSON merge patch of the Lease's `spec` (take it, renew it, release it), always with the `resourceVersion` it read, so that one writer wins each version |

- **The same in both scopes.** This rule is not in `kausa.rules` and does not follow
  `rbac.scope`: those rules are about the namespaces kausa watches, while the Lease is kausa's own
  object in its own namespace. A namespaced release gets it even when its own namespace is not
  listed, and nothing else there.
- **Helm owns the object, kausa its `spec`.** kausa never creates the Lease: RBAC cannot bind
  `create` to a name, so a grant to create would let kausa create any Lease name in the namespace.
  A deleted Lease is a `lease error` with the hint `the chart creates this Lease: run helm
  upgrade`, and no replica acts until it is back. `helm uninstall`, or going back to
  `replicaCount: 1`, deletes the Lease, the Role and the RoleBinding.
- **`patch` is not narrowed to `spec`.** RBAC cannot limit a patch to some fields, so the token can
  also change the Lease's labels and annotations (checked in kind, Kubernetes v1.36.4, on
  2026-10-29). kausa sends only `metadata.resourceVersion` and `spec` fields.
- **Nothing else changes.** The rows above, the ClusterRole or the Roles of `kausa.rules`, and
  what is never granted are the same with one replica or two.

## Who reads the history

With the history on and `history.installCRD`, the chart also installs a ClusterRole
`<full name>-explanation-reader` with `get`, `list` and `watch` on `explanations`, labelled
`rbac.authorization.k8s.io/aggregate-to-view: "true"`. Kubernetes aggregates it into the built-in
`view` role, so whoever has `view`, `edit` or `admin` in a namespace reads its Explanations as they
read its Events. No write is aggregated: only kausa and cluster administrators write Explanations.
`history.readers.aggregate: false` leaves the reader role out, and reading to cluster
administrators and to whatever the operator binds. The reader role is bound to nobody and grants
kausa nothing.

What the kind test asks `kubectl auth can-i`, as kausa's service account
(`system:serviceaccount:<release namespace>:<full name>`):

- **`cluster`**: every row above `yes` with `-A`, and its list of what must be refused (`get` and
  `list` on Secrets and ConfigMaps, `create` and `delete` on Pods, `pods/exec`, `update` and
  `delete` on Events, `list` on nodes, service account tokens, impersonation, `list` on
  ReplicaSets and Jobs, `watch` on ReplicaSets, `get`, `update`, `patch` and `delete` on
  Deployments, `get` on `deployments/scale`, `update` on `deployments/status`, and `list` and
  `watch` on StatefulSets and DaemonSets; with the history, `get`, `watch`, `update` and `deletecollection` on
  `explanations`, and `create` and `patch` on `customresourcedefinitions`) `no` with `-A`.
- **`namespaced`**: every row above `yes` with `-n` of each listed namespace, and `no` with `-n`
  of a namespace that is not listed, `-n` of kausa's own namespace, and `-A`; the list of what must
  be refused `no` in a listed namespace. `kubectl get clusterroles,clusterrolebindings -l
  app.kubernetes.io/instance=<release>` lists nothing but the reader role, which the history
  adds, and nothing at all once the history is off.
- **The Deployment watch**, in both scopes: `list` and `watch` on `deployments.apps` are among
  the rows, and kausa is Ready while `get` on them is refused. In namespaced scope, a listed
  namespace whose RoleBinding is removed also gives a `deployment watch error`. With `rollouts:
  false` (the cluster scope's upgrade in the test), the ClusterRole has no `deployments` rule,
  `list deployments.apps` is refused, and kausa runs with `--no-rollouts` and is Ready.
- **The Lease**, in both scopes: with two replicas, `get` and `patch` on `leases/<full
  name>-leader` in kausa's own namespace `yes`, and `no` in every other namespace and with `-A`;
  with one replica, `no` everywhere. Always `no`, in every namespace and with `-A`: `create` (with
  and without the name), `update`, `delete`, `list`, `watch` and `deletecollection` on Leases, and
  `get` and `patch` on another Lease or without a name. Because `kubectl auth can-i` answers `yes`
  to a `create` with a name when a rule grants `create` with `resourceNames` (seen in kind on
  2026-10-29), the test also sends a real `kubectl create --dry-run=server` of a Lease as kausa,
  under a new name and under the Lease's own, and expects it refused.
- **The readers**, in both scopes: service accounts bound to `view` and to `edit` in a watched
  namespace can `list` Explanations and cannot `create` or `delete` them.

## Never granted

- `secrets` and `configmaps`, in any verb. `list` on Secrets would return their data.
- Any write verb other than `create` and `patch` on Events, with the history `create`, `patch`
  and `delete` on `explanations`, and with two replicas `patch` on kausa's one Lease.
- On Deployments, anything but `list` and `watch`: no `get`, `create`, `update`, `patch`,
  `delete` or `deletecollection`, and no `deployments/status`, `deployments/scale` or
  `deployments/rollback`. No verb on `statefulsets`, `daemonsets` or `cronjobs`, and no `list` or
  `watch` on `replicasets` (`get` only).
- `pods/exec`, `pods/attach`, `pods/portforward`, `pods/eviction`, `nodes`, `nodes/proxy`,
  `serviceaccounts/token`, impersonation, and wildcards (`*`) in groups, resources or verbs.
- Any custom resource but kausa's own Explanations, and on those never `get`, `watch`, `update`,
  `deletecollection` or a subresource (there is none).
- Any verb on CustomResourceDefinitions: the chart installs the CRD, kausa never does.
- On Leases, `create`, `update`, `list`, `watch`, `delete` and `deletecollection`, in any
  namespace; `get` and `patch` on any Lease but `<full name>-leader` in kausa's own namespace;
  any grant on Leases with one replica; and no Lease rule in a ClusterRole.

## The pod that runs kausa

The chart's defaults:

- One replica by default, `strategy: Recreate`, so that a rollout never runs two instances at
  once. With `replicaCount: 2` two Pods run, still with `Recreate`, and only the holder of the
  Lease acts ([In kausa's own namespace, with two
  replicas](#in-kausas-own-namespace-with-two-replicas)). The election is kausa's own code, on
  the monotonic clock of each replica: a standby takes the Lease once it has seen it unchanged for
  30 s (`LEASE_DURATION`), and a leader acts only until 20 s (`RENEW_DEADLINE`) after it sent its
  last good renewal, and every cluster write and webhook attempt ends within 10 s
  (`WRITE_BOUND`). So a deposed leader's last write ends before a standby may take over. Two
  overlaps remain, both hypotheses with no fencing token: the API server applying a request
  after the client gave up on it, and a paused VM whose monotonic clock does not advance. Their
  worst case is a duplicate: one more call, Event series patch or new Event, history merge and
  notification for an incident that still fails, never a change to a workload. With one replica,
  a node partition can still briefly run two instances, as before (hypothesis, with the same
  worst case). Scale only through `replicaCount`: `kubectl scale` on a one-replica release runs
  Pods that do not elect a leader, and each would act.
- A PodDisruptionBudget `<full name>` with two replicas only: `maxUnavailable: 1`, so a drain
  never evicts both, and `unhealthyPodEvictionPolicy: AlwaysAllow`, so a replica that is not
  Ready never blocks a drain.
- `runAsNonRoot`, a numeric non-root user, `readOnlyRootFilesystem`, all capabilities dropped,
  `allowPrivilegeEscalation: false`, the `RuntimeDefault` seccomp profile.
- Requests and limits for CPU and memory declared: requests of 10m and 32 MiB, limits of 200m
  and 128 MiB, well above the 2 MiB and 1m measured on a Raspberry Pi 4 in dry-run ([Memory and
  CPU on a Raspberry Pi 4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)), to leave
  room for larger clusters and for the explanation path.
- The service account token is mounted, because kausa talks to the API server; the only other
  volumes are the optional Secret with the provider's API key, mounted read-only as a file by the
  kubelet (or, outside the chart, passed as `KAUSA_API_KEY`), and, with notifications on, the
  Secret `notifications.existingSecret` with the webhook URL and the optional bearer token,
  mounted read-only at `/var/run/secrets/kausa-notify/` (mode 0440), so kausa needs no permission
  on Secrets. That volume is never optional: a missing Secret or key leaves kausa's Pod in
  `ContainerCreating` with a `FailedMount` event on the Pod (`MountVolume.SetUp failed for volume
  "notify" : secret "kausa-notify" not found`, or `references non-existent secret key: url`), and
  the same Pod starts, with no restart, once the Secret is fixed (seen in kind, Kubernetes
  v1.36.4, on 2026-10-29). kausa takes its
  settings as command-line flags in its Pod spec: whoever can edit kausa's Deployment chooses
  where it sends data, so write access to it belongs to cluster administrators only.
- An optional NetworkPolicy that limits egress to the API server, DNS, the configured model
  endpoint and, with notifications on, the webhook (on TCP 443 unless other ports are given): by Pod and namespace selector for an endpoint in the cluster, and by IP block and port
  otherwise. Standard NetworkPolicy cannot select a hostname, so a hosted endpoint whose addresses
  change needs a CNI with FQDN policies or an egress proxy to be limited to that host.
