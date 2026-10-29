# 0014. Keep each incident's full explanations in a namespaced Explanation resource, off by default

- Status: accepted
- Date: 2026-10-28
- Builds on: [0001. Build a small, read-only explainer that writes one redacted explanation per
  incident](0001-scope-and-positioning.md) (its v0.2 roadmap row), [0008. Send only allowlisted,
  redacted data to one OpenAI-compatible endpoint](0008-data-leaving-the-cluster.md) (unchanged),
  [0009. Run with read-only permissions that never include Secrets](0009-least-privilege-rbac.md),
  and [0013. Offer a namespaced install mode: one watch pair and one Role per listed namespace, and
  nothing cluster-wide](0013-namespaced-install-mode.md), which is implemented first
- Amends: 0009 ("The only write is Events", and "Custom resources, in v0.1" under Never granted in
  [Permissions](../reference/permissions.md)), 0001 (the out-of-scope line "Changing anything in
  the cluster other than writing Events") and invariant 2 of
  [ARCHITECTURE.md](../../ARCHITECTURE.md). 0001 and 0009 stay `accepted` and get an `Update`
  paragraph when this ADR is accepted.
- Related: [0015. Post each new explanation to one optional webhook, as JSON, Slack or
  Telegram](0015-notifications.md), reviewed in the same pull request; issue #13

## Context

In v0.1.0 an explanation is an Event named `kausa.<fingerprint>.<hash of the note>`, whose note is
cut to 1024 bytes (Kubernetes' `NoteLengthLimit`) although the answer holds up to 300 characters of
summary, 600 of cause and five steps of 300, and which the API server deletes after one hour by
default (`--event-ttl`). The full text is also in kausa's log, with every namespace's real names. So
the next morning `kubectl` shows nothing. 0001's roadmap planned history in a custom resource for
v0.2, and issue #13 asks for it, with an ADR first on what is stored, retention and permissions. On
2026-10-28 FedeAM chose the API group `kausa.fedeam.com`, one docs pull request for the three v0.2
ADRs before any code, and this one implemented after 0013 and before 0015.

The code gives: a fingerprint with no uid (FNV-1a over the detection, namespace, owner and
normalized message), stable across restarts and recreation but not collision-proof; one
explanation at a time in one slot, one replica; `Explained` outcomes from a fresh answer or a
deduplication cache hit (at most 1024 entries, `--dedup-hours`), never in `--dry-run`; and kube
4.2.0 already has every call needed. The chart requires Kubernetes 1.32.

## Decision

**Nothing new leaves the cluster.** The history goes to the API server only, and nothing stored is
read back into a prompt, the cache, an Event, a notification or a log line. [Data sent to the
provider](../reference/data-sent-to-the-provider.md) gets one sentence saying so.

### The resource

- CRD `explanations.kausa.fedeam.com`, version `v1alpha1` only, kind `Explanation`, **namespaced**
  in the failing workload's namespace, with no short names or categories.
- **One object per fingerprint**, named `kausa.<fingerprint>` (`kausa-<fingerprint>` if the dot is
  refused), labelled `app.kubernetes.io/managed-by: kausa`. kausa appends to and deletes only
  "kausa-shaped" objects: labelled, named `kausa.` and 16 lowercase hexadecimal digits, with a
  `spec` that parses. This is a convention, not proof of authorship.
- **No `ownerReferences`**: the history outlives the workload on purpose (a failed Job with a TTL
  is gone minutes after it fails), and kausa's sweep deletes it.
- **Structural schema** in `charts/kausa/files/explanation-schema.json`, used by the CRD template
  and the Rust tests; every string and list capped at kausa's own caps (counted in characters),
  every field described. `spec` is required and immutable for everyone (CEL `self == oldSelf`,
  message `spec is immutable`). Its fields (`rule`, `target` and target's `apiVersion`, `kind`,
  `name`) are required, and so are all nine fields of each entry.

| Field | Type and limit | Content |
|---|---|---|
| `spec.rule` | `^D[0-9]{3}$` | The detection id |
| `spec.target.apiVersion`, `.kind`, `.name` | at most 253, 63, 253 | The top-level owner; no uid |
| `status.occurrences`, `status.lastTime` | integer at least 1, date-time | Writes since creation, and the last |
| `status.entries` | at most 5, newest first | One per distinct text |
| `entries[].summary`, `.cause` | at most 300, 600 | Whole, names mapped back |
| `entries[].steps` | at most 5 of at most 300 | Whole; may be empty |
| `entries[].confidence` | `low`, `medium`, `high` | |
| `entries[].model`, `.promptVersion` | at most 253, 16 | `--model` (cleaned) and `PROMPT_VERSION` at the last write |
| `entries[].firstTime`, `.lastTime`, `.count` | date-time, integer at least 1 | This text's first and last write, and count |

- Times are RFC 3339, UTC, to the second. Never stored: the context, logs, event messages, the
  request, the raw answer, the placeholder mapping, the endpoint, token counts, the Pod name.
  **The answer's caps do not change**: "longer" means not cut to 1024 bytes.
- **Printer columns** `KIND`, `TARGET`, `RULE`, `COUNT`, `LAST`, `AGE`; `-o wide` adds `CONFIDENCE`
  and `SUMMARY` of `entries[0]`, dropped if that path is refused.
- **Schema changes** within `v1alpha1` are additive; a breaking one is a new version without a
  conversion webhook, or lets Helm replace the CRD and its objects. Each is a CHANGELOG line.

### Writes and retention

- **Only `Explained` outcomes**, fresh or cached; skipped explanations stay Events, logs and
  counters. **After the Event and independent of it**; `--no-events --history` writes history only.
- **One read-modify-write** under a 10-second timeout: `list` by name; if absent, `create`; if
  present, **refuse** unless it has the label and the incident's `spec`, else a JSON merge patch of
  `status` and `metadata.resourceVersion` with `fieldValidation=Strict`. No other `metadata` is
  written. A `409` counts as a conflict and is not retried.
- **The merge** is pure: the same text (summary, cause, steps and confidence all equal) bumps its
  `count`, `lastTime`, `model` and `promptVersion`, keeps `firstTime`, and moves first; a new text goes first; entries older than `--history-days` go, then the list is cut
  to 5. **Stored entries are untrusted** (invariant 5): malformed, future-dated, oversized or
  unclean ones (changed by `prompt::clean`) are dropped; nothing read back is logged or sent;
  nothing panics.
- **Failures never stop anything**: a warning and a counter. A `404` names the missing CRD and
  `history.installCRD`; kausa neither exits nor probes at startup, and a CRD installed later works
  without a restart.

| Limit | Set by | Default |
|---|---|---|
| Entries per object | constant `MAX_ENTRIES` | 5 |
| Age, by `status.lastTime` (else `creationTimestamp`) | `--history-days`, `history.days`, 1 to 30 | 7 |
| Objects per kausa | constant `MAX_OBJECTS` | 500 |

- **One knob**: how long text with real names stays readable is the operator's call; the other two
  protect etcd and change only by review.
- **The sweep** takes the explanation slot at start and hourly, ahead of the queue, for at most 30
  seconds. It lists labelled objects in pages of 50, skips those not kausa-shaped, deletes those
  past the age and, through a bounded heap, the oldest beyond `MAX_OBJECTS`, each delete with a
  `resourceVersion` precondition; its memory stays under about 150 KB.
- **No pruning while kausa is down** or runs without `--history`. Between sweeps the count can pass
  `MAX_OBJECTS` by `--max-calls-per-hour` plus the cached fingerprints. An object stays under about
  55 KB, 500 under about 28 MB. The cap is shared across namespaces: it protects etcd, not fairness.

### Permissions

Only while kausa writes history, through 0013's `kausa.rules` (ClusterRole, or each listed
namespace's Role): `list`, `create`, `patch` and `delete` on `explanations` in `kausa.fedeam.com`.
Never `get`, `watch`, `update`, `deletecollection`, a subresource, another resource, or **any verb
on `customresourcedefinitions`**: the chart installs the CRD, kausa never does.

**Invariant 2 becomes:** "The only writes are Events (`create`, and a `patch` of the series of an
Event kausa wrote) and, with `--history`, Explanations (`create`, a `patch` of the `status` and
`resourceVersion` of one that carries kausa's label and the incident's `spec`, and, for retention,
a `delete` of one that carries kausa's label, a `kausa.<fingerprint>` name and a `spec`). kausa
never changes a workload and never acts on the model's answer."

**Readers:** a ClusterRole `<full name>-explanation-reader` (`get`, `list`, `watch`) aggregated to
`view`, so `view`, `edit` and `admin` read history as they read Events. No write is aggregated;
`history.readers.aggregate: false` leaves reading to cluster administrators.

### Flags and chart

- `--history`, off by default; exit `2` without `--endpoint` or with `--dry-run`. `--history-days
  <N>`, 1 to 30, default 7; exit `2` otherwise or without `--history`. The help's "writes nothing
  but Events" adds "and, with --history, its own Explanation objects".
- Values `history.enabled: false`, `history.days: 7`, `history.installCRD: true`,
  `history.readers.aggregate: true`. The helper `kausa.writesHistory` (enabled, an endpoint, no
  `dryRun`) renders the flags and the rule; `kausa.validate` checks `history.days`.
- **The CRD is a template**, not in `crds/`, rendered with `kausa.writesHistory` and `installCRD`,
  **without `helm.sh/resource-policy: keep`**: uninstalling, disabling, removing the endpoint or
  `dryRun=true` deletes it and every Explanation. With two releases, exactly one installs it.
  Uninstalling that release, or turning its history off, deletes every release's history. A
  namespaced install creates something cluster-scoped only with the history on; an installer
  without cluster rights sets `installCRD=false` and has an administrator apply the CRD and reader
  role, whose lifetime is then the administrator's. `NOTES.txt` shows how to read the history.
- Notifications (0015) and history never wait for or stop each other; the Event and the cache do
  not change.
- **Code:** a new `src/history.rs`, a fuzzed pure part and a writer over `Api<DynamicObject>`, with
  no new crate; counters
  `kausa_history_writes_total{outcome="created|updated|refused|conflict|error"}`,
  `kausa_history_deleted_total{reason="age|cap"}` and `kausa_history_sweep_errors_total`; log
  lines `history write failed`, `history write refused`, `history sweep failed`, `history swept`.

## Alternatives

- **A longer Event or TTL:** both limits are the API server's. **kausa's log:** one stream with
  every namespace's names. **A ConfigMap:** 0009 forbids ConfigMaps in every verb.
- **Cluster-scoped, or all in kausa's namespace:** wider read grants, tenants seeing each other, and
  no Role-based namespaced mode.
- **One object per workload or per explanation:** no match with the fingerprint, or no single
  history per incident.
- **A `status` subresource:** a second call and rule for what CEL gives. **No `spec`:** a buggy
  kausa could rewrite the target. **`ownerReferences`:** a Job's history gone in minutes, and the
  sweep is needed anyway.
- **kube `derive` with `schemars`:** build-time macros for one small, explicitly capped schema.
- **The CRD in `crds/`:** never upgraded or deleted. **`resource-policy: keep`:** real names kept
  with nothing pruning them. **A sweep-only mode:** a third mode for derived data.
- **kausa installs its CRD:** a cluster-wide power. **Exit at startup without it:** a crash loop
  against invariant 6.
- **Retention in flags:** large settings without review. **Loading the cache from the history:**
  editable objects become input. **On by default:** a CRD, new writes and kept text everywhere.

## Consequences

- Operators read full explanations with `kubectl` for up to `--history-days`; restarts still cost
  calls. **Off by default**, so a v0.1 upgrade with default values changes nothing.
- **Threat model** for [SECURITY.md](../../SECURITY.md): a compromised kausa can rewrite or delete
  Explanations, or add `finalizers` that stall deletions, in its namespaces, and nothing more; a
  ResourceQuota bounds the count. Whoever can create Explanations can forge kausa-shaped objects
  that fill the shared cap, but not make kausa write their text back. Text with real names and the
  `--model` value stay readable for days by Event readers; backups copy them (hypothesis).
- Installing with history needs rights to create a CRD and a ClusterRole; turning it off deletes
  all history, including other releases' sharing the CRD. Each explanation costs two API calls; no
  new egress, so the NetworkPolicy does not change.
- **Docs in the implementation pull request:** ARCHITECTURE.md, AGENTS.md, SECURITY.md,
  [Permissions](../reference/permissions.md), [Helm chart](../reference/helm-chart.md),
  [Command line and log format](../reference/command-line.md),
  [Architecture](../explanation/architecture.md),
  [Install kausa with Helm](../how-to/install-with-helm.md), a new
  `docs/reference/explanation-resource.md`, Data sent to the provider, `NOTES.txt`, `values.yaml`,
  `--help`, the README and the CHANGELOG.
- **Tests:** unit tests of the pure part (bodies, merge, untrusted input, sweep plan, schema limits
  equal to the constants); a fuzz target; a `src/run.rs` test that the sweep runs under a full
  queue; CLI usage errors; chart renders per toggle; and an e2e run in both scopes covering
  objects, columns, `can-i`, readers, CEL and `409` refusals, the sweep, a deleted CRD and upgrades.
- **Hypotheses, checked in kind before the code merges:** the dot in names; CEL rules; `required`
  on update; `maxLength` in characters; `[0]` in printer columns; `status` kept on create;
  `resourceVersion` as a patch precondition; the `view` aggregation. Also: etcd's 1.5 MiB limit;
  Helm refusing a second release's CRD and creating CRDs first (a missing CRD is only a warning);
  RBAC escalation as in 0013.

Update 2026-10-28: the sweep lists pages of 2, not 50, and its memory is bounded by the page, not
by about 150 KB. The schema bounds an object's `spec` and `status` but not its `metadata`, so an
object anyone allowed to create Explanations makes with kausa's label can reach etcd's limit,
about 1.5 MiB, and kube buffers and parses a whole page: one such object took about 12 MiB once
parsed, and a page of 50 would pass the chart's 128Mi limit and crash-loop kausa at every start.
A page of 2 stays under about 30 MiB, and the sweep drops the `metadata` it does not read before
it reduces each object. The 55 KB per object holds for objects kausa writes only
([Explanation resource](../reference/explanation-resource.md)).
