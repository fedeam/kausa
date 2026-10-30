# 0013. Offer a namespaced install mode: one watch pair and one Role per listed namespace, and nothing cluster-wide

- Status: accepted
- Date: 2026-10-28
- Builds on: [0009. Run with read-only permissions that never include
  Secrets](0009-least-privilege-rbac.md), which planned this mode without deciding it, and [0001.
  Build a small, read-only explainer that writes one redacted explanation per
  incident](0001-scope-and-positioning.md), whose roadmap puts it in v0.2
- Related: [0008. Send only allowlisted, redacted data to one OpenAI-compatible
  endpoint](0008-data-leaving-the-cluster.md) (unchanged by this decision); [0014. Keep each
  incident's full explanations in a namespaced Explanation resource, off by
  default](0014-explanation-history.md) and [0015. Post each new explanation to one optional
  webhook, as JSON, Slack or Telegram](0015-notifications.md), the other v0.2 ADRs, reviewed in the
  same pull request

## Context

Status as of 2026-10-28 (v0.1.0): kausa watches Pods and Warning Events on `Api::all`, with a
ClusterRole only; every other call is already namespaced, in a namespace the watches returned.
0009 records the cost: Event writes in every namespace, whole Pods listed cluster-wide (inline
environment values included), and `pods/log` on any container. Issue #14 asks for "a namespaced
install mode, with a Role per watched namespace instead of the ClusterRole", and for an ADR on how
the namespaces are chosen and what happens to owners and Events across namespaces. FedeAM decided
on 2026-10-28 that v0.2 adds it, before the history and the notifications. It depends on:

- **The watch state assumes one stream per kind:** merged as is, one namespace's relist would drop
  the others' records, and kausa would be ready before every namespace listed.
- **kube-runtime 4.2.0 has no multi-namespace watcher,** a field selector cannot OR namespaces, and
  a list across all namespaces needs a ClusterRole. Each watch is one HTTP/1.1 connection, and
  watch errors never end a stream (`default_backoff()`, 0.8 to 30 seconds; logged and counted).

## Decision

### Choosing the namespaces

- **`kausa run --namespace <NAME>`, repeatable.** With it, kausa watches only those namespaces;
  without it, the whole cluster exactly as in v0.1. The scope never comes from the kubeconfig's
  context or the service account's namespace file. Help line:
  `  --namespace <NAME>     Watch only this namespace; repeat for more (up to 32)`.
- **Usage errors (exit code `2`)**, before any connection: a missing or empty value (`--namespace
  needs a value`), a name that is not a DNS-1123 label, a repeated name, more than 32.
- **The cap of 32** (64 connections) is a guard. **Measuring it is a condition of shipping:** a
  kind run with 32 namespaces records memory (start, steady state) and time to Ready against the
  chart's 128Mi limit, in a research note; if not well under it (an OOM loop resets the budget), the
  mode ships with a cap of 16 or 8 until a measurement raises it.
- **No other way to choose:** no environment variable, no namespace label selector, no "all
  except" list.

### Watching

- **One Pod watcher and one event watcher per listed namespace** on `Api::namespaced`, same
  selector and backoff (cluster mode: one per kind on `Api::all`), merged per kind with `futures`'
  `select_all` and tagged with their watch: no new crate, no new task.
- **A watch that ends stops kausa**, with exit code `1` and `the pod watch of namespace <name>
  ended` (`the pod watch ended` in cluster mode), as today.
- **Relists per watch.** On `InitDone` of namespace `n`, only the records of `n` its relist did
  not see are dropped. `EventRecord` gains no field (invariant 8 holds).
- **A `403` drops that watch's records** (both modes): kausa can no longer see them go away. Other
  errors keep them, so a transient failure does not reset the settle window.
- **Readiness:** `/readyz` answers `200` once every watch (2, or 2 × N) has sent `InitDone`, `503`
  `starting` before, and never goes back. **Unchanged:** deletes, pruning, the settle window, the
  fingerprint, and one queue, budget and deduplication cache per process, shared by all namespaces.

### Events, owners and logs stay in the incident's namespace

This mode relies on a check both modes get from a separate fix: `EventRecord::from_event` drops an
Event whose `regarding.namespace` differs from its `metadata.namespace`, or that has none. An incident's namespace is then always
that of a watched Pod or Warning Event, so owner lookups (owner references cannot cross
namespaces, per the garbage collection documentation, not checked again), the log tail and the
Event write stay there: a listed namespace in namespaced mode, and in cluster mode the
cross-namespace path above is closed. `owner.rs`, `explain.rs` and `events.rs` do not change.

### The chart

- **New values:** `rbac.scope` (`cluster` or `namespaced`, required, no default) and
  `rbac.namespaces` (list, default `[]`; each namespace must exist before `helm install`).
- **`scope: cluster`** renders the ClusterRole and ClusterRoleBinding with v0.1's rules, and no
  `--namespace`. **`scope: namespaced`** renders nothing cluster-scoped: for each listed namespace,
  a Role and a RoleBinding named with the release's full name (subject: kausa's service account in
  the release namespace), and one `--namespace` argument. One list drives both, so the watch and
  the permissions cannot drift apart.
- **One set of rules:** a `kausa.rules` helper rendered by the ClusterRole and every Role, with the
  existing conditions on `pods/log` and the Event writes. `templates/clusterrole.yaml` becomes
  `templates/rbac.yaml`.
- **No default, so a reset fails closed.** `helm upgrade --reset-values` (and very probably a plain
  `helm upgrade --set image.tag=…` without `--reuse-values`; hypothesis) renders the chart's
  defaults. With a `cluster` default, an installer with cluster-admin rights would silently swap
  the Roles for a ClusterRole and widen kausa to every namespace, `kube-system` included. With no
  default the render fails and Helm applies nothing.
- **The cost:** every install and every upgrade from v0.1 sets `rbac.scope`, `--reuse-values`
  included once (v0.1's stored values have no `rbac` map, hypothesis; the templates read it through
  `default dict`). `CHANGELOG.md` (`Changed`), the release notes and `NOTES.txt` say so.
- **`kausa.validate` refuses to render**, with the reason, when the scope is unset (`rbac.scope is
  not set: set rbac.scope=cluster to let kausa read every namespace, or rbac.scope=namespaced with
  rbac.namespaces`) or another value; `namespaced` has an empty list; `cluster` has a list (a
  narrowed watch on a cluster-wide role is not least privilege); a name is not a DNS-1123 label or
  repeats; or there are more than 32.
- **The chart creates no namespace** (uninstall would delete their workloads); everything else
  stays in the release namespace. **`NOTES.txt`** names the scope and namespaces; in cluster scope
  its first lines say kausa can read Pods, Events (and logs) in every namespace.

### Permissions

No verb, resource or row of [Permissions](../reference/permissions.md) changes; only where they
apply. Pods (`get`, `list`, `watch`), `pods/log` (`get`), Events (`list`, `watch`, `create`,
`patch`), ReplicaSets and Jobs (`get`) apply in every namespace with `cluster`, and in the listed
namespaces only with `namespaced`, where no ClusterRole or ClusterRoleBinding is created. Nothing
outside the list (not `kube-system`, not kausa's own namespace unless listed), nothing on
`namespaces`. `/version` needs no grant (hypothesis: `system:public-info-viewer`). 0009's caveat
narrows but stays: in a listed namespace RBAC cannot limit Event writes to kausa's own Events.

### Log and metrics

`started` gains `namespaces` (sorted, or `null` for the whole cluster); `pod watch error` and
`event watch error` gain `namespace` (`null` cluster-wide). No metric, label or `/readyz` text
names a namespace: the metrics port is open by default.

### When a namespace or a permission is missing

- **A listed namespace that does not exist** fails the Helm install or upgrade before kausa runs.
- **A missing grant in a listed namespace:** that watch fails with `403`, is logged and counted,
  retried at most about every 30 seconds, and its records are dropped; the other namespaces keep
  working (invariant 6); kausa never exits or widens its scope. Before the first list it never
  becomes ready: with `metrics.enabled` (default) `helm install --wait` times out, while `/healthz`
  avoids restart loops. An open watch may run until its 290-second timeout (hypothesis).
- **A watched namespace deleted later** takes its Role; recreated, it gets no access until `helm
  upgrade`. Outside the chart, a missing namespace is watched as empty (hypothesis).

### Upgrades, several releases, later features, data

- Switching scope or the list is a `helm upgrade`; the Pod is replaced, restarting budget and
  cache. A namespaced install keeps its values in a file, or uses `--reuse-values`. Uninstall
  removes every Role and RoleBinding.
- Releases may share a cluster only with disjoint lists; overlapping lists, or namespaced beside
  cluster scope, would explain the same incident twice and patch each other's Event series.
- **Later features** put every permission they add into `kausa.rules`. 0014's
  CustomResourceDefinition is cluster-scoped; 0014 decides how it fits: history off by default,
  and the CRD installed only when it is on. 0015's destinations are not expected to need a
  Kubernetes permission; a namespaced release notifies only about its namespaces.
- **Nothing new leaves the cluster.** [Data sent to the
  provider](../reference/data-sent-to-the-provider.md) does not change; the namespace is still a
  pseudonym (`R002`), and the `--namespace` values appear only in the Pod spec and kausa's log.

## Alternatives

- **Derive the scope from the list,** or **`cluster` as the default** with the reset documented:
  a reset would silently restore the ClusterRole. **`rbac.allowClusterScope`:** two values for one.
- **A `lookup` of existing Roles:** empty under `helm template`, so CI cannot test it, and it
  guards only while the Roles exist.
- **One cluster-wide watch narrowed by a selector or in `record.rs`:** narrows what kausa looks at,
  not what it may read, which is the point of issue #14.
- **One ClusterRole bound per namespace:** cluster-scoped, so a namespace administrator cannot
  install it, and it suggests cluster-wide access.
- **One namespace only (`WATCH_NAMESPACE`):** N releases, budgets, keys and Pods to secure.
- **A namespace label selector:** a cluster-scoped permission on `namespaces`, and Roles that
  static templates cannot follow and kausa cannot create (invariant 2).
- **"All except":** needs a cluster-wide watch. **`--namespaces a,b`:** needs splitting rules.
- **A startup `SelfSubjectAccessReview`:** says what a `403` says, and can go stale. **Exit on a
  forbidden namespace:** one tenant stops all (invariant 6). **Ready after one lists:** hides one.
- **A `namespace` metric label or names in `/readyz`:** exposes names on an open port; it can come
  later if asked for. **`rbac.create: false`:** not asked for.

## Consequences

- **Least privilege by namespace:** a compromised kausa reads Pods, logs and Events, and writes
  Events, only in the listed namespaces. No invariant changes.
- **Installing without cluster-admin, perhaps.** Hypothesis, not claimed in the docs until tried:
  the installer must hold every permission a Role grants in each namespace; whether the default
  `admin` ClusterRole covers the Event writes is not checked.
- **Tenants of a listed namespace can change kausa's access there.** Deleting the RoleBinding
  (new) keeps the release not Ready after its next restart, so `helm upgrade --wait` times out and
  `--atomic` rolls back, security fixes included; the way out is to remove the namespace or
  restore the binding, found through the watch error lines. The readiness rule may be revisited.
  Editing the Role (new) gives a stolen token more; compare it with `helm get manifest`
  (hypothesis: the next upgrade restores it). Binding more was already possible.
- **A values reset fails the render** instead of widening kausa; the price is one required value.
  **Cluster mode changes too:** cross-namespace Events are no longer read (`Fixed` or `Security`).
- **Two connections per namespace,** not measured at 32 yet (see the cap). The budget, queue and
  cache stay shared per release, and the log holds every watched namespace's explanations.
- **Tests:** `tests/cli.rs` covers valid, repeated and every invalid `--namespace`, and the help
  line. Unit tests in `src/run.rs`: per-namespace relists, `403` drops, readiness over every
  watch, an ended watch; in `src/record.rs`: the namespace check. The `ci` target renders both
  scopes and every refusal, including default values and `--set rbac=null`, without a nil-pointer
  error. The e2e becomes a matrix `scope: [cluster, namespaced]` (`KAUSA_E2E_SCOPE`). The
  namespaced run checks: no Event or incident in an unlisted namespace; `kubectl auth can-i` per
  namespace and `-A`; no ClusterRole; a reset upgrade fails and leaves the Roles; a deleted
  RoleBinding keeps the Pod not Ready with 0 restarts while another namespace keeps working;
  uninstall leaves no Role. The cluster run adds a cross-namespace Event (settling the hypothesis
  above) and an upgrade to namespaced. Hypothesis: the matrix doubles minutes, not wall time.
- **Documentation, in the same change:** Permissions, Helm chart, Command line and log format,
  Install kausa with Helm (`--set rbac.scope=cluster`, the v0.1 upgrade, "Watch only some
  namespaces"), Try kausa against a cluster with a local model (namespaced proxy paths),
  Architecture, Set up a development environment, the [Security policy](../../SECURITY.md) (Event
  caveat, budget per release, tenant notes), `ARCHITECTURE.md`, `values.yaml` comments, the README
  and `CHANGELOG.md`. 0009 gets an `Update YYYY-MM-DD:` paragraph on acceptance pointing here.

Update 2026-10-28: the drop on a `403` needs a relist to undo it, and kube-runtime 4.2.0 relists
only after a failed list: a refused resumed watch (`WatchStartFailed`) or a `403` inside an open
stream keeps its resource version, and once access is back the watcher resumes from it, so a Pod
that did not change in between (`D004`, `D005`, `D009`) would never come back. kausa therefore
starts a new watcher after any `403`, which lists again, and keeps the default backoff (0.8 to 30
seconds) across those watchers: the `Init` of each new one does not reset the delay. No new crate;
`Restarting` in `src/run.rs`, with unit tests over scripted watchers.

Update 2026-10-29: [0018. Explain a Deployment past its progress deadline (`D014`) from a
Deployment watch that is on by default, and add nothing for StatefulSets or
DaemonSets](0018-stuck-rollouts.md) adds a third watch, on Deployments, unless `--no-rollouts`:
one on the whole cluster, or one per listed namespace, with the same per-watch relists, the same
drop of that watch's records on a `403`, and the same new watcher after it. `/readyz` turns `200`
once every watch has listed: 3, or 3 × N (2 and 2 × N with `--no-rollouts`), and an ended
Deployment watch stops kausa with `the deployment watch ended` (`… of namespace <name> ended`).
The cap of 32 namespaces is kept by the measurement with 96 watches in
[Ninety-six watches](../research/2026-10-29-96-watches.md).

Update 2026-10-29: [0019. Offer an optional standby replica that takes over through a
chart-created Lease, elected by kausa's own code](0019-high-availability.md) changes three things here, only with the chart's `replicaCount: 2`. A namespaced release is
granted one thing in its own namespace even when that namespace is not listed: `get` and `patch`
on its one Lease, `<fullname>-leader`, and nothing else there. That rule is the exception to
"later features put every permission they add into `kausa.rules`": it is about kausa's own
object, not about a watched namespace, so it lives in `templates/leader-election.yaml`, the same
in both scopes. And Ready means more than "every watch has listed": a standby is Ready after its
first successful read of the Lease leaves it waiting (`/readyz` answers `standby`), and a promoted
standby stays Ready while it lists. `/readyz` still never goes back to `503`. With one replica,
all of this section's rules hold as written.
