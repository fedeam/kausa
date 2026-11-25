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
  `dryRun` nothing of it renders. The `ci` target of the `Dockerfile` and `tests/chart/render.sh`
  check the renders on and off and that the CustomResourceDefinition's schema equals
  `charts/kausa/files/incident-schema.json`.
- **The kind test** gained the Incident cases of point 8 (`tests/e2e/run.sh`). **They have never
  run**: no cluster and no Docker were used in this session.
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
