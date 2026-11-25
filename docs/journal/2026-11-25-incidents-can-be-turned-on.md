# 2026-11-25 — Incidents can be turned on

## Request

Make the Incidents of [0028. Publish each incident as an immutable Incident that only kausa
writes, and report by webhook what kurita proposed for it, both off by
default](../adr/0028-incident-resource-and-remediation-report.md) something a user can turn on, in
the binary and in the chart, without changing anything with the defaults, and document them.

## What was done

- **The defaults pinned first**, before any switch existed:
  - `tests/golden/metrics-start.prom`, the metrics exposition at start with the defaults, checked
    by a test in `src/run.rs`;
  - `tests/golden/chart-default-cluster.yaml` and `tests/golden/chart-default-namespaced.yaml`, the
    chart's default render of each scope with its version replaced by `0.0.0-golden`, checked by
    `tests/chart/render.sh`.
- **The flags**: `--incidents` and `--incident-days <N>` (1 to 30, 1 by default). `--incident-days`
  needs `--incidents`, and `--incidents` is a usage error with `--dry-run`. The gate's window is
  `--dedup-hours`. The four `kausa_incident*` counters show from 0 only with `--incidents`, so the
  default exposition is unchanged.
- **The chart**: `incidents.enabled`, `incidents.days`, `incidents.installCRD` and
  `incidents.readers.aggregate`. One helper, `kausa.writesIncidents`, gives the flags, the rule
  (`create`, `list` and `delete` on `incidents`, last in kausa's rules) and the
  CustomResourceDefinition, so no value grants the rule to a kausa that does not use it. With
  `dryRun` nothing of it renders. `tests/chart/render.sh` checks the renders on and off and that
  the CustomResourceDefinition's schema equals `charts/kausa/files/incident-schema.json`; the `ci`
  target of the `Dockerfile` is written to check them too, but it was never built (below).
- **The kind test** gained the Incident cases of point 8 (`tests/e2e/run.sh`). **They have never
  run**: no cluster and no Docker were used in this session. Nor were the image build (`docker
  build --target ci .`, with the chart renders and the checks it runs inside) and the three
  `promtool` commands of `AGENTS.md`: `promtool` is not installed on the machine used.
- **Two edits to 0028** where the code showed its text wrong: the `starting` refusal applies until
  any sweep of the process has ended, and the unverified parts (the hourly sweep rests on a pure
  test; a success answer other than `201` that carries a UID is taken as a create) are listed.
  0028 stays `proposed`.
- **The documentation**: a new [Incident resource](../reference/incident-resource.md), and
  [Permissions](../reference/permissions.md), [Helm chart](../reference/helm-chart.md), [Command
  line and log format](../reference/command-line.md), [Alerts](../reference/alerts.md), [Install
  kausa with Helm](../how-to/install-with-helm.md), [Set up a development environment and run the
  checks](../how-to/develop.md), the README, the [Security policy](../../SECURITY.md),
  [Architecture](../explanation/architecture.md), the front page, `AGENTS.md`, invariants 2, 5 and
  12 of [ARCHITECTURE.md](../../ARCHITECTURE.md) and the changelog.

- **A correction round after review**, the same day:
  - a test of the sweep with `--incident-days 3`, which fails if the age rule uses one day;
  - the chart refuses a `budget.dedupHours` that is not a whole number from 0 to 1000000 whenever
    it passes `--dedup-hours`, naming the value;
  - `NOTES.txt` gains an Incidents paragraph, rendered only while the release writes Incidents;
  - the pages name the three uses of the one `--dedup-hours` window, and say that a
    `budget.dedupHours` set without an endpoint now applies, with incidents on, to the `detected`
    notifications and to the gate;
  - the kind restart case tells the new process's Incidents by `metadata.uid`, and each subject
    of the new `can-i` cases answers yes once before its "never" rows;
  - invariant 5 names what the sweep does with the listed namespace, name and `continue` token;
  - 0028's "Not verified" cites `sweep_due` at its line and lists two assumptions of the kind
    cases; the front page links the Incident resource; long lines are rewrapped.
  The image build, `promtool` and the kind cases did not run in this session.
- **Then the pull request's CI ran them** at `d048b40`, the round's last commit, and every job
  passed: the checks on both architectures (the `Dockerfile`'s `ci` target, with the three
  `promtool` commands), the image on both, the fuzz job, the kind test in both scopes and with the
  standby, and the upgrade test in both scopes. Each Incident case of point 8 printed its result
  line in both scopes. [0028](../adr/0028-incident-resource-and-remediation-report.md)'s "Not
  verified" says what they showed.

## Decisions

- None new: everything follows 0028, which stays `proposed`. Where 0028 is silent, the reading
  taken: the counters come after every other counter in the exposition; with `dryRun` the chart
  renders the Incidents off instead of refusing them; the README's principle keeps its title and
  gains the Incidents in its text.

## Follow-ups

- Run the kind test in both scopes (and the upgrade test) on GitHub Actions before 0028 is
  accepted.
- The remediation report of 0028 (point 6) is not implemented: no flag, value, rule or watch on
  `remediations`.
