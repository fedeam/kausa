# Permissions

The Kubernetes API permissions kausa's chart grants, and the ones it never asks for.

Status as of 2026-10-23: **planned** for v0.1 (no chart exists yet). Why it is this list: [0009.
Run with read-only permissions that never include Secrets](../adr/0009-least-privilege-rbac.md).
The kind tests in CI will check the chart's role against this table. As of 2026-10-26,
`kausa run` uses the read rows: the Pod and event watches, the metadata `get` of ReplicaSets and
Jobs, and `pods/log` for the detections that read logs; it writes no Events yet. On 2026-10-26 it
ran against a live cluster through a `kubectl proxy` that let only these reads through, and the
proxy refused none of its requests ([Try kausa against a cluster with a local
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

Granted through a ClusterRole by default. A namespaced mode with a Role per watched namespace is
planned ([0001. Build a small, read-only explainer that writes one redacted explanation per
incident](../adr/0001-scope-and-positioning.md)).

## Never granted

- `secrets` and `configmaps`, in any verb. `list` on Secrets would return their data.
- Any write verb other than `create` and `patch` on Events.
- `pods/exec`, `pods/attach`, `pods/portforward`, `pods/eviction`, `nodes`, `nodes/proxy`,
  `serviceaccounts/token`, impersonation, and wildcards (`*`) in groups, resources or verbs.
- Custom resources, in v0.1.

## The pod that runs kausa

Planned defaults of the chart, measured and adjusted on the fourth weekend:

- One replica, `strategy: Recreate`, so that a rollout never runs two instances at once; a node
  partition can still briefly run two (hypothesis: the worst case is a duplicate Event, since
  kausa never changes a workload). Leader election with a Lease is not planned for v0.1.
- `runAsNonRoot`, a numeric non-root user, `readOnlyRootFilesystem`, all capabilities dropped,
  `allowPrivilegeEscalation: false`, the `RuntimeDefault` seccomp profile.
- Requests and limits for CPU and memory declared; the starting values are hypotheses until the
  measurement on a Raspberry Pi 4.
- The service account token is mounted, because kausa talks to the API server; the only other
  volume is the optional Secret with the provider's API key, mounted read-only as a file by the
  kubelet (or passed as `KAUSA_API_KEY`), so kausa needs no permission on Secrets. kausa takes its
  settings as command-line flags in its Pod spec: whoever can edit kausa's Deployment chooses
  where it sends data, so write access to it belongs to cluster administrators only.
- An optional NetworkPolicy that limits egress to the API server, DNS and the configured model
  endpoint: by Pod and namespace selector for an endpoint in the cluster, and by IP block and port
  otherwise. Standard NetworkPolicy cannot select a hostname, so a hosted endpoint whose addresses
  change needs a CNI with FQDN policies or an egress proxy to be limited to that host.
