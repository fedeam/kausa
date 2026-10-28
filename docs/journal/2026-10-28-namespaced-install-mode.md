# 2026-10-28 — Namespaced install mode

## Request

Build the namespaced install mode that [0013. Offer a namespaced install mode: one watch pair and
one Role per listed namespace, and nothing cluster-wide](../adr/0013-namespaced-install-mode.md)
accepted, in three tracks (code, chart, docs) on the branch `feat/v0.2-namespaced`, and check it:
the measurement the ADR makes a condition of shipping, both kind runs, and the ADR's hypotheses
that a kind cluster can settle.

## What was done

- **The Event namespace check came first,** on its own branch and merged to `main` as #38:
  `EventRecord::from_event` drops an Event whose `regarding.namespace` differs from its
  `metadata.namespace`, or that has none. The namespaced mode relies on it, and cluster mode gets
  it too (the `Security` line in the changelog).
- **Code:** `kausa run --namespace <NAME>`, repeatable up to 32 (`MAX_NAMESPACES`), with its four
  usage errors, checked before any connection. `src/run.rs` builds one Pod watch and one event
  watch per namespace (or one of each on the whole cluster), tags each item with its watch, and
  merges them per kind with `futures`' `select_all`, so the loop keeps one Pod branch and one
  event branch and no new crate or task. Relists and `403` drops are per watch, readiness waits
  for every watch (2 or 2 × N), and an ended watch names its namespace. `started` gains
  `namespaces` and the watch error lines `namespace`. Unit tests in `src/run.rs` and cases in
  `tests/cli.rs`.
- **Chart:** `rbac.scope` (`cluster` or `namespaced`, required, no default) and `rbac.namespaces`;
  `templates/clusterrole.yaml` became `templates/rbac.yaml`, which renders the ClusterRole or one
  Role and RoleBinding per listed namespace from one `kausa.rules` helper; `kausa.validate`
  refuses the seven bad cases first, nil-safe; one `--namespace` per entry in the Deployment;
  `NOTES.txt` names the scope. The Dockerfile's `ci` target and the new `tests/chart/render.sh`
  render both scopes and every refusal. Every `helm install` and `helm lint` in the repository
  now sets `rbac.scope` (the README, the docs, `AGENTS.md`, the Dockerfile, `tests/e2e/run.sh` and
  the install command printed by `release.yml`).
- **The kind test is a matrix** of `KAUSA_E2E_SCOPE` (`cluster`, `namespaced`) in
  `.github/workflows/e2e.yml`, so its checks are now named `kind (cluster)` and
  `kind (namespaced)`.
- **Integration:** the chart track was merged into the code track, then `main` (ADRs 0013 to 0016,
  `D009` and `D010`, and #38) was merged in; the changelog's `Security` line, which both sides
  carried, was kept once. Then the docs: [Permissions](../reference/permissions.md), [Helm
  chart](../reference/helm-chart.md), [Command line and log
  format](../reference/command-line.md), [Install kausa with
  Helm](../how-to/install-with-helm.md), [Try kausa against a cluster with a local
  model](../how-to/try-with-a-local-model.md), [Architecture](../explanation/architecture.md),
  [Set up a development environment and run the checks](../how-to/develop.md), the [Security
  policy](../../SECURITY.md), `ARCHITECTURE.md`, the README, the changelog, and an `Update` in
  [0009. Run with read-only permissions that never include
  Secrets](../adr/0009-least-privilege-rbac.md).

## How it was checked

- `cargo fmt`, `clippy -D warnings` and `cargo test` on the integrated branch; `helm lint
  --strict` in both scopes; `tests/chart/render.sh` (every check passed); `gitleaks` on the
  clone (no leaks); `docker build --target ci .`.
- **The shipping gate:** [Thirty-two namespaces](../research/2026-10-28-32-namespaces.md). With 32
  namespaces (64 watches) kausa used about 13 MiB in steady state and 23 MiB at its peak, against
  the 128 MiB limit, and was ready half a second after it started. The cap stays at 32.
- **Both kind runs passed on the maintainer's machine**, on the integrated branch: `cluster` (the
  v0.1 checks, the cross-namespace Event, the upgrade to `namespaced`) and `namespaced` (an
  unlisted namespace with the same failures left alone, `kubectl auth can-i` per namespace and
  with `-A`, nothing cluster-scoped, the reset upgrade refused with the Roles kept, a refusing
  namespace that keeps kausa not Ready with 0 restarts while another is explained, and an
  uninstall that leaves no Role). Each run used a kubeconfig file of its own; the maintainer's
  kubeconfig had the same checksum before and after, and every kind cluster was deleted.

## Hypotheses of 0013, settled in kind

All with Kubernetes v1.36.4 and Helm 3.16.1, on 2026-10-28:

- **The cross-namespace Event:** the API server accepted an `events.k8s.io/v1` Event in one
  namespace whose `regarding` object is in another, and kausa ignored it (no incident, no Event).
  So the check was needed.
- **`/version` needs no grant:** in namespaced scope, with nothing cluster-wide granted, kausa
  read the server version and became ready. Which default ClusterRole allows it was not checked.
- **v0.1's stored values have no `rbac` map:** `helm upgrade --reuse-values` from the v0.1.0
  chart failed with `rbac.scope is not set`, without a new revision; with `--set
  rbac.scope=cluster` it passed, and the ClusterRole's rules were identical to v0.1.0's. Later
  `--reuse-values` upgrades carried the scope on.
- **A plain `helm upgrade --set …`** (neither `--reuse-values` nor a values file) rendered the
  defaults and failed the same way. Confirmed.
- **A missing listed namespace** fails `helm install` with `namespaces "<name>" not found`, and
  Helm keeps the release as `failed`; uninstalling it left no Role.
- **An edited Role** (a rule on ConfigMaps added) was put back by `helm upgrade` with the same
  values; so was a deleted RoleBinding, after which the watch errors stopped.
- **An open watch outlives its RoleBinding:** after the binding was deleted, the first watch
  error came 289 seconds later, which fits the watch's 290-second timeout ending it. Its `error` text
  says `forbidden` and the `403`. kausa stayed Ready (readiness never goes back) and did not
  restart.
- **Outside the chart, a namespace that does not exist** is watched as an empty one: with broad
  credentials, `kausa run --namespace <existing> --namespace <missing>` became ready with no
  watch error.
- **Not settled here:** installing without cluster-admin (whether the `admin` ClusterRole covers
  the Event writes), and whether the matrix doubles CI minutes but not wall time. Both need
  another setup or GitHub Actions.

## Found on the way

- With `--reuse-values`, going back from `namespaced` to `cluster` needs an explicit empty list:
  `--set-json 'rbac.namespaces=[]'` or a values file with `namespaces: []` worked, while `--set
  rbac.namespaces=null` and `--set 'rbac.namespaces={}'` left a list (`{}` renders as one empty
  name) and the chart refused. [Install kausa with Helm](../how-to/install-with-helm.md) says so.

## Follow-ups

- **For the maintainer:** if branch protection on GitHub requires a check named `kind`, replace
  it with `kind (cluster)` and `kind (namespaced)`, or no pull request can merge (hypothesis: the
  required check is named after the job). Both matrix jobs still have to pass on GitHub Actions.
- The release notes of the next release must say that `rbac.scope` is required, as the
  changelog's `Changed` line does.
