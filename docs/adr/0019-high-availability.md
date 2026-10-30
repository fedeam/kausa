# 0019. Offer an optional standby replica that takes over through a chart-created Lease, elected by kausa's own code

- Status: accepted
- Date: 2026-10-29
- Builds on: [0009. Run with read-only permissions that never include
  Secrets](0009-least-privilege-rbac.md), [0013. Offer a namespaced install mode: one watch pair
  and one Role per listed namespace, and nothing cluster-wide](0013-namespaced-install-mode.md),
  [0014. Keep each incident's full explanations in a namespaced Explanation resource, off by
  default](0014-explanation-history.md) and [0015. Post each new explanation to one optional
  webhook, as JSON, Slack or Telegram](0015-notifications.md)
- Amends: 0001 (out of scope: "changing anything in the cluster other than writing Events"), 0009
  ("the only write is Events", Never granted), [0010](0010-packaging.md) (one replica), 0013 ("not
  kausa's own namespace unless listed", "every permission into `kausa.rules`", what Ready means),
  invariants 1, 2, 5, 6 and "A new permission" of [ARCHITECTURE.md](../../ARCHITECTURE.md), and
  [Permissions](../reference/permissions.md) ("Leader election with a Lease is not planned"). Each
  stays `accepted` and gets an `Update` paragraph when this ADR is accepted.
- Related: [0008](0008-data-leaving-the-cluster.md) (unchanged), and the other v0.4 ADRs,
  `0017-richer-context-for-the-model.md` and `0018-stuck-rollouts.md` (see Consequences)

## Context

Status as of 2026-10-29 (v0.3.0): the chart runs one replica with `strategy: Recreate`. When its
node dies, nothing is explained until the Pod is replaced: about five minutes with the default
`tolerationSeconds` of 300 (hypothesis), then start-up and the settle window. FedeAM asked for two
replicas, of which only a leader watches, explains and writes. The code gives:

- **No leader election in kube 4.2.0.** `k8s-openapi` 0.28.0 (`v1_32`) has
  `coordination::v1::Lease`; its `strategy` is Alpha, behind `CoordinatedLeaderElection`.
- **One thread, state in memory.** A `current_thread` runtime, no tokio `sync` feature, `Cell` and
  `RefCell` in `Metrics` and `Shared`; the loop awaits owner lookups inline (5 s per hop). The
  records, tracker, queues (16 each), delayed retries, `Budget` and `Dedup` (1024) are per process.
- **Writes survive a duplicate.** Event names are deterministic and a `409` patches the series of
  any Event kausa wrote, whatever its `reportingInstance`; Explanation writes carry
  `resourceVersion` preconditions. `Writer::emit` has no timeout; history and webhook calls, 10 s.

Checked in a throwaway kind cluster (Kubernetes 1.36.4, Helm 3.16.1) on 2026-10-29: a Role rule on
`leases` with `resourceNames` and `get`, `patch` allows exactly those and nothing on other names or
namespaces; with `create` and `resourceNames`, `kubectl auth can-i create leases/<name>` says `yes`
while the real create is refused. A merge patch with a stale `metadata.resourceVersion` gets `409`;
`holderIdentity: ""` is accepted, `leaseDurationSeconds: 0` and negative transitions are not. Helm
installs the Lease (an unknown kind) after the Deployment; label-change, no-op and `--reset-values`
upgrades keep a written `spec`; an upgrade recreates a deleted Lease and uninstall removes it.
`helm install --wait` waits for both replicas of a `Recreate` Deployment. A PodDisruptionBudget
with `unhealthyPodEvictionPolicy: AlwaysAllow` is accepted.

## Decision

### Off by default; one value, one flag

- **Chart value `replicaCount`**, `1` (default) or `2`. With `1`, or no key (`--reuse-values` from
  v0.3), nothing new is rendered. `kausa.validate` reads it with `hasKey` and refuses anything else:
  `replicaCount must be 1 or 2, not '<value>'`.
- **`kausa run --leader-lease <NAMESPACE>/<NAME>`**: act only while holding that Lease, a standby
  otherwise, in every mode (log-only and `--dry-run` too). Usage errors (exit `2`, before any
  connection): no value, not one `/` between a DNS-1123 label and subdomain, given twice, or
  `POD_NAME` unset, empty or over 253 bytes. The identity is `POD_NAME`, compared byte for byte.

### The election (`src/leader.rs`, pure part in `src/leader/pure.rs`)

- **Calls:** `get`, and a JSON merge patch of `spec` carrying the `metadata.resourceVersion` last
  seen, so one writer wins each version. No new crate.
- **Constants**, not flags; a test asserts `RENEW_DEADLINE + WRITE_BOUND <= LEASE_DURATION`:

| Constant | Value | Meaning |
|---|---|---|
| `LEASE_DURATION` | 30 s | A standby takes the Lease once it has seen it unchanged this long |
| `RENEW_DEADLINE` | 20 s | The leader acts until this long after it *sent* its last good renewal |
| `RETRY_PERIOD` | 5 s | The leader renews and the standby reads this often |
| `CALL_TIMEOUT` / `RELEASE_TIMEOUT` | 5 s / 2 s | Each Lease call / the release at shutdown |
| `WRITE_BOUND` | 10 s | The longest a cluster write or a webhook attempt may run |

- **Monotonic time only.** A replica records when it first saw the current `resourceVersion`. The
  Lease is free when `holderIdentity` is empty or absent, is its own, or has not changed for
  `LEASE_DURATION`. `renewTime`, `acquireTime` and `leaseDurationSeconds` are written for readers
  and never read, so neither clock skew nor an edited value moves an expiry.
- **Acquire:** `holderIdentity`, `leaseDurationSeconds: 30`, `acquireTime`, `renewTime`, and
  `leaseTransitions` (stored, clamped to `0..=i32::MAX`, plus one when the holder changes).
  **Renew:** `holderIdentity` and `renewTime`, with the version the last patch returned; after a
  `409` it reads once and, still named, renews (a Helm label change), else steps down. **Release**
  (signal, or a watch that ends), once the controller has stopped: `holderIdentity: ""`,
  `leaseDurationSeconds: 1`.
- **A missing Lease** (`404`) is a `lease error` whose hint is `helm upgrade`. kausa never creates
  one: with no Lease nobody leads (fail closed).
- **Untrusted:** only `holderIdentity` is compared and the version echoed; nothing read from the
  Lease is logged, sent or reused, and parsing never panics.

### What each replica does

- **Both** connect, log `started`, serve `/metrics`, `/healthz`, `/readyz`, and stop on signals.
- **The standby** reads the Lease and nothing else: no watch, owner lookup, model call or write.
- **The leader** runs v0.3.0's controller body in one top-level `tokio::select!`; no task is
  spawned. It is `biased` only for the signals, then the keeper. The last arm is a nested,
  unbiased select of the controller and the metrics `accept` and connections, the random share
  `src/run.rs:416` gives them today, so a flood on the metrics port (any peer while
  `networkPolicy.metricsFrom` is empty) cannot starve the controller while the keeper keeps
  renewing and the standby never takes over. The keeper ends on another holder or on its own
  deadline timer, so a hung call cannot delay it, and the controller future is dropped with
  everything in flight.
- **The term guard.** `Shared` carries the term's end, checked before a budget take and model call,
  each Event create, list or patch, history write, sweep list or delete, and webhook attempt; past
  it the call is skipped silently. In every mode, `Writer::emit` gets a 10 s timeout and each sweep
  delete ends at `min(sweep deadline, its start + 10 s)`.
- **Stepping down in place, never exiting for it:** `leadership lost`, then the records, tracker,
  both queues, delayed retries and owner cache are dropped. **`Budget` and `Dedup` (with
  `notified`) are kept**: kausa's own memory, not cluster input. A watch that ends still exits `1`.

### Readiness, metrics, log

- **`/readyz`:** `503 starting` until the first role check, then `200` and never `503` again
  (0013): `standby` after the first successful Lease read, `ready` once a leader's watches have all
  listed. A promoted standby stays `200` while it lists. A first install still waits for the leader
  to list, since `helm --wait` waits for both replicas.
- **Metrics,** no label naming a Pod, namespace or Lease: gauge `kausa_leader` (1 leading or
  without the flag, 0 standby; the first gauge), counters `kausa_leadership_changes_total{to}`
  (`leader`, `standby`) and `kausa_lease_errors_total{call}` (`get`, `patch`; `409` not counted).
- **Log:** `started` gains `leader_lease` (`"<ns>/<name>"` or `null`); info `standby`, `leading`,
  `lease released`; warn `leadership lost` (`reason`: `not renewed in time`, `held by another`) and
  `lease error` (`call`, `error`, a `hint` on `403` and `404`), logged when it starts or changes and
  at most once a minute while it lasts, counted every time.

### Permissions

Only with `replicaCount: 2`: a Role and RoleBinding `<fullname>-leader` in the release namespace,
the same in both `rbac.scope`s, in `templates/leader-election.yaml`, outside `kausa.rules`:

```yaml
- apiGroups: ["coordination.k8s.io"]
  resources: ["leases"]
  resourceNames: ["<fullname>-leader"]
  verbs: ["get", "patch"]
```

- **No `create`** (RBAC cannot bind it to a name), **no `update`** (a `PUT` sends back the metadata
  it read), no `list`, `watch`, `delete`, other Lease, namespace or cluster-scoped grant.
- **Invariants:** 1 adds `get` and `patch` on one named Lease in kausa's own namespace, in either
  scope; 2 adds a merge patch of that Lease's `spec` under a `resourceVersion` precondition; 5 adds
  the Lease to what is never logged, sent or reused. "A new permission" gains this exception.
- **Invariant 6** no longer holds unqualified: "nothing the cluster, the endpoint or the webhook
  does may stop the watches" gains "without `--leader-lease`", and the invariant gains: "With
  `--leader-lease`, only the holder of the Lease acts; losing it stops the watches in that replica
  and hands them to the other, and never ends the process. Three things stop the watches in every
  replica with no handover, failing closed as scaling to zero does: whoever can write kausa's Lease
  in the release namespace holding it under another identity (or changing its version faster than
  `LEASE_DURATION`, so no standby takes over from a dead leader); a deleted Lease or
  `<fullname>-leader` RoleBinding; and Lease calls failing for `RENEW_DEADLINE`, as in an API server
  outage, which also drops the records and settle progress v0.3.0 kept. One call per incident
  becomes one per incident per process: a failover, forced or not, may explain a still-failing
  incident once more."

### The chart, with `replicaCount: 2`

- `replicas: 2`, **`Recreate` kept**, and `--leader-lease <release namespace>/<fullname>-leader`.
- A Lease `<fullname>-leader` with the chart's labels and `spec: {}`: Helm owns the object, kausa
  its `spec`. Uninstalling, or going back to `1`, deletes it.
- PodDisruptionBudget `<fullname>`: `maxUnavailable: 1`, `unhealthyPodEvictionPolicy: AlwaysAllow`.
- While `affinity` is empty, a preferred pod anti-affinity (weight 100, `kubernetes.io/hostname`,
  selector labels). Any `affinity` replaces it, a required one included.
- `NOTES.txt` names the Lease and how to see its holder. NetworkPolicy, Service, probes unchanged.
- **Data:** nothing new leaves the cluster (the Lease holds a Pod name and times): [Data sent to the
  provider](../reference/data-sent-to-the-provider.md) and [Data sent in
  notifications](../reference/data-sent-in-notifications.md) do not change.

## Alternatives

- **A crate** (`kube-leader-election`, `kube-lease-manager`; internals not checked here): a new
  dependency, `create` on Leases, a logger beside `log.rs`, for two calls and one pure function.
- **kausa creates the Lease:** `create` cannot be bound to a name, so a stolen token could
  pre-create other controllers' lock names; the Lease would outlive uninstall.
- **`update`:** writes back metadata read from the cluster. **Expiry from `renewTime`:** depends on
  clocks and on values anyone who can write the Lease edits.
- **Exit on a lost Lease:** each loss resets the budget, so whoever flips the Lease multiplies the
  spend, and an API server outage pushes the Pod toward `CrashLoopBackOff`.
- **A spawned keeper task:** needs `Send` state; a top-level select suffices.
- **One `biased` select with the controller last:** a metrics-port flood keeps `accept` or a
  connection ready on almost every pass, so the controller is rarely polled while the keeper
  renews. **The controller before the metrics arms:** a busy relist could delay `/healthz`, which
  today's random share does not.
- **client-go's 15/10/2 s** (recalled, not re-read): write timeouts would drop to 5 s, for 2.5 times
  the Lease traffic and a takeover that the 60 s settle window dwarfs.
- **`RollingUpdate`:** the first upgrade from v0.3.0 could run its unelected Pod beside a leader,
  and versions would lead in turn. **Required anti-affinity:** a one-node cluster leaves a Pod
  `Pending` and `helm --wait` fails (checked).
- **A hot standby:** whole Pods streamed into an idle process, to save a relist of about a second.
  **The budget or cache in the Lease, the history or a ConfigMap:** editable input (0014).
- **Election always on, N replicas, timing flags, two values, `/readyz` `503` on a standby or on
  promotion:** a grant for every install, settings nothing measured asks for, or rollouts and
  `helm --wait` that never finish. **Coordinated leader election:** Alpha.

## Consequences

- **Availability:** a crashed or partitioned leader is replaced within 35 s
  (`LEASE_DURATION + RETRY_PERIOD`) of its last renewal, a drained or deleted one in about 5 s via
  the release; then a relist (0.5 to 1.6 s measured) and the settle window. Upgrades keep the gap.
- **A failover loses** the old leader's records, settle progress, queues, retries, call in flight,
  owner cache and counters. An incident still failing is explained again (one call, a series patch
  or a new Event, a merged history entry, a second notification); one that ended in the gap is
  not. History keeps the record across it and seeds nothing.
- **Bounds:** writes start before `sent + 20 s` and end by `sent + 30 s`, no later than a takeover.
  Hypotheses: the API server does not apply a request after the client gave up, and a paused VM's
  monotonic clock advances; there is no fencing. Each process keeps its own budget, so a release
  spends at most twice the caps while both processes live; restarts reset them, as today.
- **Threat model** ([SECURITY.md](../../SECURITY.md)): a stolen token can hold or flip its one Lease
  (and patch its labels), nothing more. Whoever can write Leases in kausa's namespace can stop it
  in every replica (fail closed, seen as `sum(kausa_leader) != 1`), as they could scale it to zero;
  the reviewer found in kind 1.36.4 that the default `edit` and `admin` ClusterRoles grant every
  verb on `leases` (not re-run here). A deleted Lease or binding stops every replica until
  `helm upgrade`. An API server outage of 20 s or more drops records and settle progress, so
  incidents settle again after it. SECURITY.md lists the first two under "Who controls kausa's
  inputs" and all three, with the second explanation per process, under "What kausa cannot
  promise". The metrics port gains no new way to stop the watches (see the leader's select).
- **0013:** a namespaced release is granted one Lease in its own namespace even when unlisted.
  **0017** changes only what the leader sends. **0018**'s Deployment watch is one more leader-only
  watch to relist at takeover, and a `D014` incident open at failover settles again.
- **Cost:** about 0.4 API requests per second and a second Pod (10m CPU, 32 MiB requested). In
  every mode: the `emit` timeout (`Changed`) and `kausa_leader` (`Added`).
- **Tests:** pure tests; `run.rs` tests over a scripted Lease (guard, step-down keeping `Budget` and
  `Dedup`, release, and a metrics listener that is always ready while the controller still handles
  its watch items); `tests/cli.rs`; `tests/chart/render.sh` (both values, both scopes, `1` equal to
  no key, every refusal); a third e2e entry (namespaced, `replicaCount=2`) with graceful, frozen and
  fail-closed handovers, `can-i` plus a real `--dry-run=server` create, and a clean uninstall.
  **Condition of shipping:** a research note with the standby's memory and takeover times in kind,
  and renewal delays and controller progress (incidents still explained) under a metrics-port
  connection flood at the 200m CPU limit. Whether tokio's cooperative budget alone would have
  left the controller a share is a hypothesis, not measured.
- **Documentation, in the implementation pull request:** Permissions, Helm chart, Command line and
  log format, Architecture, Install kausa with Helm (per-Pod scraping, alerts), SECURITY.md,
  ARCHITECTURE.md, AGENTS.md, `values.yaml`, `NOTES.txt`, `--help`, README and CHANGELOG.

Update 2026-10-29: the condition of shipping is met in [Standby and
failover](../research/2026-10-29-standby-and-failover.md), measured on kind. The standby used
0.6 MiB of working set (4.4 MiB resident) and 0.07m CPU; the two replicas made 23.8 Lease calls a
minute (0.4 a second). A deleted leader released the Lease within 0.09 s, and on one node its
replacement Pod, not the standby, took it 0.44 to 0.80 s after the delete in five runs out of five;
with the replacement held `Pending`, the standby took it in 1.5 to 4.2 s. A frozen leader was
replaced 27.7 to 32.7 s after `SIGSTOP`, 30.0 to 35.14 s after its last renewal: once 0.14 s past
the 35 s of Availability above, because the standby's 30-second check fell a millisecond short and
waited one more read; the worst case is `LEASE_DURATION + 2 × RETRY_PERIOD`, as the unit tests
assert. Under a metrics-port flood that kept the leader at its 200m limit, renewals were at most
24 ms late, the standby never took over and failing Jobs were still explained, 4 to 5 s later;
relisting 10,000 Pods, at most 165 ms late with a flood. The decision is unchanged; the bound
of Availability above reads 40 s (`LEASE_DURATION + 2 × RETRY_PERIOD`) from here on.
