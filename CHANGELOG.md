# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Each release also attaches `kausa-X.Y.Z.intoto.jsonl`, the SLSA build provenance of the chart and
  `SHA256SUMS`, which `gh attestation verify --bundle` checks.
- `D009`, pod evicted: a Pod the kubelet evicted in the last 10 minutes (its `ephemeral-storage`
  limits, an `emptyDir` `sizeLimit`, or node pressure) is an incident of its workload, without
  reading logs; the kubelet refusing a new Pod on a node under pressure is not. The request to the
  model now carries each Pod's `status.reason` and `status.message`, both redacted ([0016. Send
  each Pod's status reason and message to the model, redacted and
  bounded](docs/adr/0016-send-the-pod-status-reason-and-message.md)).
- `D010`, pod sandbox cannot be created: the kubelet's Warning event `FailedCreatePodSandBox` about
  a Pod (a `RuntimeClass` handler the nodes do not have, for example), settled like `D006` and
  `D007` and grouped by the Pod's owner, without reading logs. Such a Pod waits in
  `ContainerCreating` and was no incident before.

### Changed

- `D005`, pod cannot be scheduled, also matches a Pending Pod whose `PodScheduled` condition is
  `False` with reason `SchedulerError` (for example a volume the scheduler cannot bind), which
  was no incident before; the incident's reason is the condition's own.

### Fixed

- A Pod in phase `Failed` (evicted, or past its `activeDeadlineSeconds`) whose container had
  restarted at least once is no longer reported as a crash loop (`D001`). Such incidents, which
  grew with every evicted replacement Pod and never cleared, stop after the update; an
  out-of-memory kill in a failed Pod is still `D003`.

### Security

- Warning events whose `regarding` object is in a different namespace from the event itself, or
  that have no namespace, are ignored, so Events created in one namespace can no longer make kausa
  group incidents, look up owners, read logs or write its Event in another.

## [0.1.0] - 2026-10-28

The first release: detection, grouping, redaction, explanations, cost controls, Events, the image
and the Helm chart. The image is `ghcr.io/fedeam/kausa:0.1.0` (linux/amd64 and linux/arm64,
signed with cosign), and the chart is attached to the GitHub release.

### Added

- `kausa run`: watches Pods and Warning events, detects crash loops, image pull failures,
  out-of-memory kills, container creation errors, scheduling failures, probe failures, volume
  failures and failed Jobs (`D001` to `D008`), groups the failures of one workload into one
  incident once they persist for the settle window (`--settle-seconds`, 60 by default), and logs
  each incident as a JSON line with a fingerprint.
- Explanations: with `--endpoint` and `--model`, each incident's context (container states, the
  incident's Warning events and, for crash loops, out-of-memory kills and failed Jobs, the failed
  container's last log lines) is redacted and sent to one OpenAI-compatible endpoint, and the
  validated explanation is logged with the real names mapped back. `--dry-run` logs the exact
  request instead. Redaction rules `R001` to `R013`, `--api-key-file` or `KAUSA_API_KEY`,
  `--allow-http`, `--no-logs` and `--no-json-schema`.
- Cost controls and output: a call budget per hour and per day (`--max-calls-per-hour`,
  `--max-calls-per-day`), a cap on a request's estimated tokens (`--max-input-tokens`), reuse of
  explanations for repeated incidents (`--dedup-hours`), the explanation written as an
  `events.k8s.io/v1` Event on the failing workload (`--no-events` to turn it off), and Prometheus
  metrics with health and readiness endpoints (`--metrics-address`).
- One retry, 30 seconds later, for a call that fails with a timeout, a failed connection, `429`
  or a `5xx` status; a call that never reached the endpoint does not count against the budget.
  The start of the endpoint's error body is logged.
- Counters for retried calls (`kausa_explanations_retried_total`) and watch errors
  (`kausa_watch_errors_total`).
- `kausa --help` and `kausa --version`.
- The controller image (`docker build --target runtime`): a static binary on distroless, non-root,
  for linux/amd64 and linux/arm64.
- A Helm chart in `charts/kausa`, with the permissions of the reference, the restricted Pod
  Security settings, the API key as a file from your Secret, a metrics Service and an optional
  NetworkPolicy.
- Documentation: scope and positioning after a competitive survey, the planned architecture, the
  detection and redaction rules, the permissions and the data sent to the model provider for
  v0.1, how to build from source and run the checks, a security policy with a threat model, a
  contributing guide, and the decision records in `docs/adr/`.

[unreleased]: https://github.com/fedeam/kausa/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/fedeam/kausa/releases/tag/v0.1.0
