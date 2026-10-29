# 0009. Run with read-only permissions that never include Secrets

- Status: accepted
- Date: 2026-10-23

## Context

A controller that explains failures needs to read what failed, and it is tempting to grant it
read access to everything. The competitive survey of 2026-10-23
([Competitive landscape](../research/2026-10-23-competitive-landscape.md)) found:

- k8sgpt's Helm chart grants `get`, `list` and `watch` on every resource of every API group,
  Secrets included, and the role its operator creates for the analysis workload reads Secrets and
  `pods/log` cluster-wide.
- kagent's tools are installed with cluster-admin by default.
- HolmesGPT and khook do not read Secrets; HolmesGPT reads ConfigMaps and `pods/log`
  cluster-wide.

The maintainer's requirements for kausa (2026-10-23): `get`, `list` and `watch` only on what is
needed; never read Secrets; `pods/log` only if needed, and documented.

## Decision

kausa's permissions are the fixed list in [Permissions](../reference/permissions.md): read Pods,
read and write Events, `get` on ReplicaSets and Jobs to follow owner references, and `get` on
`pods/log` only while log collection is on. Nothing else. Proposed while the repository was set
up on 2026-10-23; FedeAM accepted it on 2026-10-26, with `pods/log` granted by default
for the crash-type and Job detections, as [0008. Send only allowlisted, redacted data to one
OpenAI-compatible endpoint](0008-data-leaving-the-cluster.md) decides.

- **Never Secrets, never ConfigMaps.** Not even `list`, which returns their data. The API key
  reaches kausa through the kubelet ([0008. Send only allowlisted, redacted data to one
  OpenAI-compatible endpoint](0008-data-leaving-the-cluster.md)).
- **The only write is Events**, `create` and `patch` (the patch updates the series count of a
  repeated explanation). No other write verb, no subresource other than `pods/log`, no `exec`,
  `attach` or `portforward`, no wildcards.
- **Owner chains with the least reads.** A Pod's owner reference already names its ReplicaSet,
  StatefulSet, DaemonSet or Job. Only two hops need a read: ReplicaSet to Deployment and Job to
  CronJob, so kausa needs `get` on `replicasets` and `jobs`, and nothing on Deployments,
  StatefulSets, DaemonSets or CronJobs.
- **`pods/log` is optional** and on by default only for crash-type detections; turning log
  collection off removes it from the chart.
- **Cluster-wide or namespaced.** The chart installs a ClusterRole by default. A namespaced mode,
  with a Role in each watched namespace, is planned; it is in v0.1 only if it fits the weekends,
  and otherwise in v0.2 ([0001. Build a small, read-only explainer that writes one redacted
  explanation per incident](0001-scope-and-positioning.md)).
- **kausa ignores its own Events**, by their reporting controller, so it never explains itself.

## Consequences

- Some explanations are less precise: kausa cannot see that a ConfigMap key is wrong, only that
  the container failed and what its log says. The explanation names what to check instead.
- kausa writes Events only in the namespaces it watches, but the default ClusterRole allows
  writing them in any namespace (the configuration narrows the watch, not RBAC); the planned
  namespaced mode limits it to the watched namespaces. `create` and `patch` on Events cluster-wide
  cannot be limited to kausa's own Events: RBAC has no filter on the reporting controller, so a
  compromised kausa could create or rewrite any Event in the namespaces it covers. kausa patches
  only Events it created, with its reporting controller, and the tests check it.
- An Event cannot change a workload, but it is visible to everyone who can read Events there, so
  its text is redacted of the values kausa removed before the request, and mapped back only to
  names that were already visible on the object.
- A permission added later (for example to read a custom resource's status) needs a new ADR and
  an update to [Permissions](../reference/permissions.md) and the [Security
  policy](../../SECURITY.md).
- The kind tests in CI (third weekend) assert the chart's role against the reference list, so the
  documentation and the chart cannot drift apart.

Update 2026-10-26: "crash-type detections" above means crash loops and out-of-memory kills, and
the logs of failed Jobs are read too (`D008`), as [0008. Send only allowlisted, redacted data to one
OpenAI-compatible endpoint](0008-data-leaving-the-cluster.md) decided when it was accepted.

Update 2026-10-28: the namespaced mode planned here is decided in [0013. Offer a namespaced
install mode: one watch pair and one Role per listed namespace, and nothing
cluster-wide](0013-namespaced-install-mode.md): the chart's required `rbac.scope` grants this
ADR's rules in every namespace (`cluster`, a ClusterRole as above) or in the listed namespaces only
(`namespaced`, a Role and a RoleBinding per namespace and nothing cluster-scoped). The rules
themselves do not change, and the caveat on Event writes narrows to the listed namespaces.

Update 2026-10-28: [0014. Keep each incident's full explanations in a namespaced Explanation
resource, off by default](0014-explanation-history.md) amends "the only write is Events": with
`--history`, kausa also writes its own Explanations (`list`, `create`, `patch` and `delete` on
`explanations` in `kausa.fedeam.com`, only while it writes history, through the same rules in
either scope). Under Never granted in [Permissions](../reference/permissions.md), "Custom
resources, in v0.1" becomes "any custom resource but kausa's own Explanations", and any verb on
CustomResourceDefinitions is never granted. Secrets and ConfigMaps stay out in every verb.
