# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Detection rule `D011`, workload cannot create Pods: a Warning `FailedCreate` event about a
  ReplicaSet, a Job, a StatefulSet or a DaemonSet (a quota, an admission webhook or Pod Security
  admission refused the Pod), an incident about the Deployment, the CronJob or the controller
  itself, settled like `D006` and `D007`.
- Detection rule `D012`, volume claim cannot be provisioned: a Warning `ProvisioningFailed` event
  about a PersistentVolumeClaim, an incident about the claim. A Pod that waits for that claim is
  still `D005`, a second incident about its workload.
- Detection rule `D013`, autoscaler cannot get metrics: a Warning `FailedGetResourceMetric`,
  `FailedGetContainerResourceMetric`, `FailedGetPodsMetric`, `FailedGetObjectMetric`,
  `FailedGetExternalMetric`, `FailedComputeMetricsReplicas` or `FailedGetScale` event about a
  HorizontalPodAutoscaler, an incident about the autoscaler.

### Changed

- `R003` redacts more names in event messages: the quoted name after `pod`, `container` and
  `StorageClass` (every name of a list), after a resource qualified by its API group
  (`deployments.apps "<name>"`), and the quota's name in `exceeded quota: <name>`.
- The request names a PersistentVolumeClaim or a HorizontalPodAutoscaler that an incident is about
  by its kind (`persistentvolumeclaim-1`, `horizontalpodautoscaler-1`), not as `Object`.

## [0.3.0] - 2026-10-29

A namespaced install mode, the explanation history and notifications to one webhook, the last two
off by default. Breaking: the chart's `rbac.scope` is required on every install and upgrade.

### Added

- A namespaced install mode ([0013. Offer a namespaced install mode: one watch pair and one Role
  per listed namespace, and nothing cluster-wide](docs/adr/0013-namespaced-install-mode.md)):
  `kausa run --namespace <NAME>`, repeatable up to 32 times, watches only those namespaces, with
  one Pod watch and one event watch each; the chart's `rbac.scope=namespaced` with
  `rbac.namespaces` grants a Role and a RoleBinding in each listed namespace, passes the same list
  as `--namespace`, and creates nothing cluster-scoped. The `started` log line names the watched
  namespaces (`namespaces`, or `null` for the whole cluster), and `pod watch error` and
  `event watch error` name the namespace of the failing watch (`namespace`).
- The explanation history, off by default ([0014. Keep each incident's full explanations in a
  namespaced Explanation resource, off by default](docs/adr/0014-explanation-history.md)):
  `kausa run --history` keeps each explained incident's whole explanations (up to five distinct
  texts, with counts and times) in an Explanation object named `kausa.<fingerprint>` in its
  namespace, and deletes those older than `--history-days` (1 to 30, default 7) or beyond 500
  objects. `--history` needs `--endpoint` and is refused with `--dry-run`. The chart's new
  `history` values (`enabled: false`, `days: 7`, `installCRD: true`, `readers.aggregate: true`)
  install the CustomResourceDefinition `explanations.kausa.fedeam.com`
  (`charts/kausa/files/explanation-schema.json`, `v1alpha1`), a reader ClusterRole aggregated to
  `view`, and the rule `list`, `create`, `patch` and `delete` on `explanations`, only while kausa
  writes history; turning it off or uninstalling deletes the CustomResourceDefinition and every
  Explanation. New counters `kausa_history_writes_total{outcome}`,
  `kausa_history_deleted_total{reason}` and `kausa_history_sweep_errors_total`, and the log lines
  `history write failed`, `history write refused`, `history sweep failed` and `history swept`.
- Notifications, off by default ([0015. Post each new explanation to one optional webhook, as
  JSON, Slack or Telegram](docs/adr/0015-notifications.md)): `kausa run --notify-url-file <PATH>`
  posts each new explanation, after its Event, to the webhook whose URL is in that file, as the
  documented `json` body (schema `kausa.fedeam.com/notification/v1`), a Slack incoming webhook's
  plain text or a Telegram `sendMessage` (`--notify-format json|slack|telegram`,
  `--notify-telegram-chat-id`, `--notify-token-file` for a bearer token with `json`,
  `--notify-allow-http`). Only the namespace's and the top-level owner's real names are sent;
  every other name stays a placeholder, and no field holds a log line or the request ([Data sent
  in notifications](docs/reference/data-sent-in-notifications.md)). Needs `--endpoint`; refused
  with `--dry-run`. The chart's new `notifications` values (`enabled: false`, `format: json`,
  `existingSecret`, `urlKey: url`, `tokenKey`, `telegramChatId`, `allowHttp: false`) mount the
  Secret that holds the URL, and `networkPolicy.notifications` (`to`, `ports`, TCP 443 by default)
  adds an egress rule; no RBAC change. New counters `kausa_notifications_total{outcome}` and
  `kausa_notifications_retried_total`, the log lines `notification sent`, `notification retried`,
  `notification failed` and `notification dropped`, and a `notifications` field in the `started`
  line. The Slack and Telegram formats are not yet tested against the real services.

### Changed

- **The chart's `rbac.scope` is required** (`cluster` or `namespaced`, no default): every
  `helm install` sets it, and so does the first `helm upgrade` of a release installed with an
  earlier chart (v0.2.0 or v0.1.0), `--reuse-values` included, since its stored values have
  none. `--set rbac.scope=cluster` keeps the ClusterRole and the whole-cluster watch. Without it,
  or with an invalid list, the chart refuses to render and Helm changes nothing, so a values
  reset can no longer widen a namespaced install to the whole cluster.
- `/readyz` answers `200` once every watch has listed: the two of the whole cluster, or the two of
  each watched namespace. A watch refused with `403` drops the records it had brought in (its
  namespace's only) and lists again once access is back.
- The chart's `templates/clusterrole.yaml` became `templates/rbac.yaml`, and its rules moved to the
  `kausa.rules` helper, which the ClusterRole and every Role render.
- `/metrics` lists three more counters, `0` unless the history is on:
  `kausa_history_writes_total`, `kausa_history_deleted_total` and
  `kausa_history_sweep_errors_total`.

### Fixed

- A text of an explanation cut at its limit no longer ends in a space or in part of a
  `<redacted:…>` marker, and cleaning a cleaned text gives the same text: a character removed
  between two others can no longer leave a pair that Unicode normalization joins.

## [0.2.0] - 2026-10-28

A security fix for Events about objects in another namespace, two new detection rules (`D009`,
`D010`), fixes to `D001` and `D005`, and the build provenance of the chart as a release asset.

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

[unreleased]: https://github.com/fedeam/kausa/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/fedeam/kausa/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/fedeam/kausa/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/fedeam/kausa/releases/tag/v0.1.0
