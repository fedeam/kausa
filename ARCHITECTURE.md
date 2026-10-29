# ARCHITECTURE.md — kausa

A map of the code for whoever changes it, person or coding agent: where each part lives, how data
flows, the invariants no change may break, and the path each kind of change takes. The design
and its reasons are in [Architecture](docs/explanation/architecture.md) and the decisions in
[`docs/adr/`](docs/adr/); how to work in this repository is in [AGENTS.md](AGENTS.md).

## The flow

```text
API server ──watch──▶ run.rs ──▶ record.rs ──▶ detect.rs ──▶ incident.rs ──▶ (settled incident)
  Pods, Warning         loop      small records   rules         signals, grouping,      │
  Events                          (objects dropped)              settle window,          │
                                                                 fingerprint.rs          ▼
                          owner.rs ◀── metadata get of ReplicaSets and Jobs        explain.rs queue
                                                                                        │
     context.rs ◀── allowlisted fields ── redact.rs (placeholders, patterns) ◀──────────┘
          │
          ▼
     prompt.rs ──▶ budget.rs ──▶ model.rs ──▶ prompt::validate ──▶ restore names ──▶ events.rs
     (request,      (calls per    (one POST,     (schema, caps,                       (an Event on
     untrusted      hour/day,     HTTPS, no      cleaning)                            the owner)
     wrapper)       dedup cache)  redirects)
```

Everything else is around it: `main.rs` parses the flags, `log.rs` writes kausa's own log,
`metrics.rs` serves the counters and the health endpoints.

## Codemap

| File | What it does | Change it when |
|---|---|---|
| `src/main.rs` | Flags, usage errors (`--namespace` checked before any connection, at most `MAX_NAMESPACES`), the settings passed to `run` | A flag is added or changed |
| `src/run.rs` | The controller loop: one Pod watch and one event watch for the whole cluster, or a pair per `--namespace`, merged per kind and tagged with their watch; relists, `403` drops and a new watcher after a `403` (`Restarting`) per watch; readiness once every watch has listed; the tick, the explanation queue, the metrics listener, owner lookups with a cache | The loop's scheduling or what it watches changes |
| `src/record.rs` | Reduces a Pod or an Event to the few fields kausa keeps; the object is dropped at once | A rule needs a field that is not recorded |
| `src/detect.rs` | The detection rules `D001`–`D010`: pure functions of a record | A detection rule is added or changed |
| `src/incident.rs` | Signals, grouping by top-level owner, the settle window | Grouping or settling changes |
| `src/fingerprint.rs` | The incident fingerprint (dedup key, Event name), never sent | What makes two incidents "the same" changes |
| `src/owner.rs` | ReplicaSet → Deployment and Job → CronJob, metadata only | A new owner hop (needs an ADR: a new permission) |
| `src/explain.rs` | One incident to one explanation: log tail, fit to the token cap, dry-run or call, retry, validation, Event | The explanation pipeline changes |
| `src/context.rs` | The context sent to the model: allowlisted fields only (`R001`), all names and text redacted | Anything sent to the model changes (needs an ADR) |
| `src/redact.rs` | The redaction rules `R002`–`R013` and the mapping back of placeholders | A redaction rule is added or changed |
| `src/prompt.rs` | The fixed request (system message, untrusted wrapper, answer schema) and the validation of the answer | The request or the answer's format changes |
| `src/budget.rs` | Call budget per hour and per day, the deduplication cache | Cost controls change |
| `src/model.rs` | The client of the one OpenAI-compatible endpoint | The HTTP call changes |
| `src/events.rs` | The `Explained` / `ExplanationSkipped` Event; a repeat patches only its series | The output changes |
| `src/metrics.rs` | The counters, `/metrics`, `/healthz`, `/readyz` | A counter is added |
| `src/log.rs` | One JSON object per line, through a serializer | Only to keep it safe |
| `charts/kausa/` | The Helm chart: RBAC by `rbac.scope` (`templates/rbac.yaml`: a ClusterRole, or a Role per listed namespace, both from `kausa.rules`), the refusals of `kausa.validate`, Deployment, Service, NetworkPolicy | Install, permissions or a flag change |
| `Dockerfile` | The `ci` target (every check) and the `runtime` image | A check, a pinned tool or the image changes |
| `tests/cli.rs` | Command-line behaviour and exit codes | A flag or exit code changes |
| `tests/chart/render.sh` | Chart checks with helm alone: both scopes, the rules against the v0.1.0 chart, `NOTES.txt`, every refusal | The chart changes |
| `tests/e2e/` | The kind test, once per `rbac.scope` (`KAUSA_E2E_SCOPE`): chart, Events, what leaves the cluster, the ClusterRole or the Roles and what they grant, an unlisted and a refusing namespace, metrics | The chart or what kausa writes changes |
| `fuzz/`, `.clusterfuzzlite/` | Fuzz targets for redaction and answer validation, run by ClusterFuzzLite | A parser of untrusted text changes, or a new one is added |
| `tests/bench/` | Seeded failures and the token-counting proxy of the benchmarks | A benchmark is repeated |

Unit tests live next to the code, in each file's `tests` module.

## Invariants

A change that breaks one of these is wrong even if every test passes. Changing one needs an
accepted ADR, and most should never change.

1. **kausa never reads Secrets or ConfigMaps**, in any verb, and asks for no permission beyond
   [Permissions](docs/reference/permissions.md) ([0009](docs/adr/0009-least-privilege-rbac.md)).
   In namespaced scope it is granted those only in the listed namespaces, and nothing
   cluster-scoped; the watch and the grant come from one list
   ([0013](docs/adr/0013-namespaced-install-mode.md)).
2. **The only write is Events**: `create`, and a `patch` of the series of an Event kausa wrote.
   kausa never changes a workload and never acts on the model's answer.
3. **Nothing reaches the model without passing `context.rs` and `redact.rs`**: only the fields of
   [Data sent to the provider](docs/reference/data-sent-to-the-provider.md), every name replaced
   and every free text redacted ([0008](docs/adr/0008-data-leaving-the-cluster.md)).
4. **Redaction only ever gets stricter.** A change that lets through a value a rule caught before
   needs an ADR.
5. **Cluster text is data, never instructions**: the model gets no tools, and its answer must
   match the schema and is cleaned before it is logged or written. Code that parses untrusted text
   (cluster text, the model's answer) never panics on any input; the fuzz targets check it.
6. **One call per incident, within the budget**, and one explanation at a time; nothing the
   cluster or the endpoint does may stop the watches. A namespace that refuses kausa (`403`) is
   logged and retried; it never stops the other namespaces, and kausa never widens its scope.
7. **Rules are pure**: `detect.rs`, `incident.rs`, `fingerprint.rs` and `redact.rs` do no I/O and
   read no clock of their own, so the same state gives the same incidents, fingerprints and
   redactions.
8. **No cache of whole objects**: watch events are reduced to records at once, and objects are
   never logged.
9. **kausa's log goes through `log.rs`**, so a value from the cluster cannot break a line.
10. **No `unsafe`** (`unsafe_code = "forbid"`), no new dependency without a reason that passes
    `cargo deny`, and everything pinned: crates by `Cargo.lock`, images by digest, Actions by
    SHA, downloads by SHA-256.

## How each kind of change goes

Every change is a pull request with its tests ([Contributing to kausa](CONTRIBUTING.md)); a
feature that is not a rule starts with an issue and an ADR.

- **A detection or redaction rule** → [Add a detection or redaction
  rule](docs/how-to/add-a-rule.md).
- **A new flag** → `main.rs` and its help; [Command line and log
  format](docs/reference/command-line.md); a case in `tests/cli.rs`; the chart's values and
  [Helm chart](docs/reference/helm-chart.md) if it belongs in a Pod.
- **A new counter or log line** → `metrics.rs` (`COUNTERS`) or the call site; [Command line and
  log format](docs/reference/command-line.md).
- **Anything sent to the model** → an ADR first; then `context.rs`, [Data sent to the
  provider](docs/reference/data-sent-to-the-provider.md) and [SECURITY.md](SECURITY.md) in the
  same change.
- **A new permission** → an ADR first; then the `kausa.rules` helper in
  `charts/kausa/templates/_helpers.tpl`, which `templates/rbac.yaml` renders in the ClusterRole and
  in every Role (never a rule in one scope only), [Permissions](docs/reference/permissions.md),
  the `kubectl auth can-i` lists in `tests/e2e/run.sh` and [SECURITY.md](SECURITY.md).
- **The chart** → [Helm chart](docs/reference/helm-chart.md), and `tests/e2e/run.sh` if what it
  installs changes.
- **A dependency** → its reason in the pull request, and `cargo deny --locked check` green.
- **A pinned tool, image or action** → [Maintain the repository](docs/how-to/maintain.md).

Then the checks of [Verification (same as CI)](AGENTS.md#verification-same-as-ci), a line in the
[Changelog](CHANGELOG.md) if users will notice, and the docs that describe what changed.
