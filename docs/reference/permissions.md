# Permissions

The Kubernetes API permissions kausa's chart grants, and the ones it never asks for.

Status as of 2026-10-28: the chart in `charts/kausa` grants this table, `pods/log` only when
kausa explains incidents (an endpoint or dry-run) with log collection on, and the Event writes
only with an endpoint, outside dry-run, unless they are turned off ([Helm
chart](helm-chart.md)). `rbac.scope` picks where: in every namespace (`cluster`), or in the listed
namespaces only (`namespaced`, not in a release yet; see [Where the permissions
apply](#where-the-permissions-apply)). Why it is this list: [0009. Run with read-only permissions
that never include Secrets](../adr/0009-least-privilege-rbac.md); why there are two scopes: [0013.
Offer a namespaced install mode: one watch pair and one Role per listed namespace, and nothing
cluster-wide](../adr/0013-namespaced-install-mode.md). The kind test (`tests/e2e/run.sh`) checks
the installed role against this table with `kubectl auth can-i`, both what it allows and a list
of what it must refuse, in each scope; both scopes passed on 2026-10-28 on the maintainer's
machine. As of 2026-10-26,
`kausa run` uses every row: the Pod and event watches, the metadata `get` of ReplicaSets and Jobs,
`pods/log` for the detections that read logs, and `create` and `patch` on Events for its
explanations (with `--endpoint`, unless `--no-events`). On 2026-10-26 it
ran against a live cluster through a `kubectl proxy` that let only these reads through, plus
`POST` and `PATCH` on Events in one test namespace, and the proxy refused none of its requests ([Try kausa against a cluster with a local
model](../how-to/try-with-a-local-model.md)).

## Granted

| API group | Resource | Verbs | Why |
|---|---|---|---|
| `""` (core) | `pods` | `get`, `list`, `watch` | Container states, restart counts, phase, owner references |
| `""` (core) | `pods/log` | `get` | The last lines of the failed container (the previous instance if it restarted) for crash-type and Job detections. Only while log collection is on |
| `events.k8s.io` | `events` | `list`, `watch` | Warning events about Pods and Jobs |
| `events.k8s.io` | `events` | `create`, `patch` | Write the explanation, and update its series count when the incident repeats |
| `apps` | `replicasets` | `get` | Follow a ReplicaSet's owner reference to its Deployment |
| `batch` | `jobs` | `get` | Follow a Job's owner reference to its CronJob |

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
| `batch` `jobs`: `get` | every namespace | the listed namespaces only |
| ClusterRole and ClusterRoleBinding | created | not created |

- **`cluster`**: one ClusterRole and one ClusterRoleBinding, as in v0.1, and kausa watches every
  namespace.
- **`namespaced`**: one Role and one RoleBinding in each namespace of `rbac.namespaces`, named
  with the release's full name and bound to kausa's service account in the release namespace;
  kausa is started with one `--namespace` per listed namespace, so the watch and the grant come
  from the same list. kausa is granted nothing outside the list: no Pod, log, Event, ReplicaSet or
  Job in `kube-system`, in another tenant's namespace, or in its own namespace unless that is
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

What the kind test asks `kubectl auth can-i`, as kausa's service account
(`system:serviceaccount:<release namespace>:<full name>`):

- **`cluster`**: every row above `yes` with `-A`, and its list of what must be refused (`get` and
  `list` on Secrets and ConfigMaps, `create` and `delete` on Pods, `pods/exec`, `update` and
  `delete` on Events, `list` on nodes, service account tokens, impersonation, and `list` on
  ReplicaSets and Jobs) `no` with `-A`.
- **`namespaced`**: every row above `yes` with `-n` of each listed namespace, and `no` with `-n`
  of a namespace that is not listed, `-n` of kausa's own namespace, and `-A`; the list of what must
  be refused `no` in a listed namespace. `kubectl get clusterroles,clusterrolebindings -l
  app.kubernetes.io/instance=<release>` lists nothing.

## Never granted

- `secrets` and `configmaps`, in any verb. `list` on Secrets would return their data.
- Any write verb other than `create` and `patch` on Events.
- `pods/exec`, `pods/attach`, `pods/portforward`, `pods/eviction`, `nodes`, `nodes/proxy`,
  `serviceaccounts/token`, impersonation, and wildcards (`*`) in groups, resources or verbs.
- Custom resources, in v0.1.

## The pod that runs kausa

The chart's defaults:

- One replica, `strategy: Recreate`, so that a rollout never runs two instances at once; a node
  partition can still briefly run two (hypothesis: the worst case is a duplicate Event, since
  kausa never changes a workload). Leader election with a Lease is not planned for v0.1.
- `runAsNonRoot`, a numeric non-root user, `readOnlyRootFilesystem`, all capabilities dropped,
  `allowPrivilegeEscalation: false`, the `RuntimeDefault` seccomp profile.
- Requests and limits for CPU and memory declared: requests of 10m and 32 MiB, limits of 200m
  and 128 MiB, well above the 2 MiB and 1m measured on a Raspberry Pi 4 in dry-run ([Memory and
  CPU on a Raspberry Pi 4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)), to leave
  room for larger clusters and for the explanation path.
- The service account token is mounted, because kausa talks to the API server; the only other
  volume is the optional Secret with the provider's API key, mounted read-only as a file by the
  kubelet (or, outside the chart, passed as `KAUSA_API_KEY`), so kausa needs no permission on Secrets. kausa takes its
  settings as command-line flags in its Pod spec: whoever can edit kausa's Deployment chooses
  where it sends data, so write access to it belongs to cluster administrators only.
- An optional NetworkPolicy that limits egress to the API server, DNS and the configured model
  endpoint: by Pod and namespace selector for an endpoint in the cluster, and by IP block and port
  otherwise. Standard NetworkPolicy cannot select a hostname, so a hosted endpoint whose addresses
  change needs a CNI with FQDN policies or an egress proxy to be limited to that host.
