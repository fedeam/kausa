# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Prometheus alerting rules for kausa's own metrics, `deploy/prometheus/kausa-alerts.yaml`: eleven
  alerts in five groups, on a failing model endpoint, failed calls, the spent daily budget, the
  explanation queue, skipped incidents, failing or refused watches, the Lease of two replicas and
  failed notifications. CI checks them with `promtool check rules` and unit-tests each threshold,
  window and `for` with `promtool test rules`. [Alerts](docs/reference/alerts.md) explains each
  alert and what to do when it fires; each alert's `runbook_url` links its section.
- Seven metrics: `kausa_build_info{version, prompt_version}` (always `1`),
  `kausa_model_call_duration_seconds{outcome}` (a histogram of the model calls, whose `_count`
  equals `kausa_model_calls_total`), `kausa_watch_relists_total{watch}` and
  `kausa_watch_restarts_total{watch}` (new watchers after a `403`), `kausa_records{kind}`,
  `kausa_owner_cache_entries` and `kausa_dedup_cache_entries`
  ([Command line and log format](docs/reference/command-line.md#metrics)). No label names a
  namespace, a Pod, a model or a host.

### Changed

- The counters with a fixed set of label values (`kausa_watch_errors_total`,
  `kausa_model_calls_total`, `kausa_explanations_skipped_total`, `kausa_notifications_total` and
  `kausa_leadership_changes_total`) show a `0` series for each value from the start, in the modes
  that can count them, so that the first increase after a start is seen; their unlabelled `0`
  line is gone, and a dashboard that matched it changes.
- The example alert on a spent daily budget, now `KausaDailyCallBudgetSpent`, matches the leader
  gauge with `ignoring(window)` instead of `on (pod)`, so it needs no `pod` label from the scrape.
- The example alert on a flapping Lease, now `KausaLeaderFlapping`, fires at 7 takeovers in an
  hour (`>= 7`) instead of `> 6`, which six takeovers could cross once `increase` extrapolates.
  It and `KausaNoLeader` add up a release's Pods `by (namespace, job)`.

### Security

- The name of a Secret or ConfigMap that a Pod cannot mount reached the request and kausa's log in
  clear when the volume was named like a secret key (`credentials`, `api-token`, `password`…):
  `R010` took the kind in the kubelet's `volume "credentials" : secret "<name>" not found` for the
  key's value, so `R003` no longer saw `secret "<name>"`. A kind followed by a quoted name is no
  longer a value for `R010`, and `R003` redacts the name. Found by the known-cause corpus's
  `no-volume` entry; a value was never exposed, only the object's name.

## [0.5.1] - 2026-11-03

A fix for the endpoint check and the breaker with Google's Gemini API, which answers a wrong key
with a `400`. Nothing to change in values or permissions.

### Fixed

- A wrong or missing API key for Google's Gemini API is `key`, a configuration error that opens
  the breaker, instead of `rejected`: its OpenAI-compatible endpoint answers it with a `400`
  (`INVALID_ARGUMENT`) and one of three fixed messages, which kausa now compares byte for byte. The
  check's hint names the key instead of `--no-json-schema`
  ([#72](https://github.com/fedeam/kausa/issues/72)).

## [0.5.0] - 2026-11-03

The model's answer passes the pattern redaction rules before it is written anywhere (0022), the
hourly call budget refills at a steady rate, so that a restart no longer refills it (0021),
`kausa check-endpoint` and the chart's `helm test` check the endpoint, and a breaker holds calls
while it fails (0020), and `D002`, `D004`, `D008` and `D011` explain more failures. Upgrade the
image and the chart together, since the new chart passes `--model-timeout`, which older images
refuse; after the upgrade an Explanation may start a new entry once.

### Added

- `R011` redacts npm (`npm_`), PyPI (`pypi-`) and Docker (`dckr_pat_`, `dckr_oat_`) tokens and
  GitLab's other token prefixes (`gldt-`, `glrt-`, `glcbt-` and the rest), each format taken from
  its issuer's documentation ([#5](https://github.com/fedeam/kausa/issues/5)).
- The model's answer passes the pattern redaction rules `R003`–`R014` before it is logged, put in
  an Event, stored in an Explanation or posted to the webhook: an address, a token, an email
  address, a URL's query or a host outside the allowlist that the model writes, from injected
  cluster text or invented, is written redacted, and kausa keeps no unredacted copy. The rules,
  their order and the details that differ from the request are in [Detection and redaction
  rules](docs/reference/rules.md#on-the-models-answer)
  ([0022](docs/adr/0022-redact-the-answer.md)).
- The counter `kausa_answer_redactions_total{rule}` and the `answer_redactions` field of the
  `explanation` log line count the values replaced in the model's answers, by rule.
- A prompt injection corpus (`tests/bench/injection-corpus.yaml`) with its measurement script, and
  seed corpora for the `redact` and `validate` fuzz targets.
- Two gauges: `kausa_budget_calls_remaining{window}` (`hour`: the calls the refill bucket allows
  now within the 60-minute cap; `day`: what is left of the 24-hour cap), rendered with
  `--endpoint`, and `kausa_explanation_queue_length` (at most 16), rendered with `--endpoint` or
  `--dry-run`. A gauge never set is not rendered.
- An `explanations waiting` log line (`reason`, `queued`, `next_call_in_seconds`), once when
  incidents start waiting for the budget, and a `waiting_for_budget` field on `explanation
  dropped` ([0021](docs/adr/0021-refill-the-call-budget.md)).
- `D008` also explains a Job failed by its `podFailurePolicy` (`PodFailurePolicy`) and an indexed
  Job with failed indexes (`MaxFailedIndexesExceeded`, `FailedIndexes`): every reason the Job
  controller gives a failed Job.
- `D011` also explains a CronJob whose Jobs the API server refuses (a quota, an admission webhook),
  from its `FailedCreate` event; the CronJob is the incident's object.
- `D002` also explains `ErrImageNeverPull`: `imagePullPolicy: Never` and an image the node cannot
  use, absent or not usable with the Pod's credentials.
- `D004` also explains a container whose process the runtime cannot start (`RunContainerError`,
  and `CrashLoopBackOff` after a `StartError`), such as an entrypoint missing from the image.
- `R003` redacts the container and the Pod named in the Job controller's `podFailurePolicy` notes.
- A known-cause corpus in `tests/bench/`: a seeded failure for every detection
  (`seeded-failures.yaml`), its cause written down before any model ran (`known-causes.json`), and
  `score.py`, which replays dry-run requests against an endpoint and grades the answers with a
  fixed rubric; first results in
  [Explanation quality of local models against known causes](docs/research/2026-11-02-explanation-quality.md).
- `kausa check-endpoint`: one synthetic request to the model endpoint, on demand, with no cluster
  and outside the budget, and one `endpoint check` line that names the step that failed, its cause
  and a hint; exit `0` passed, `1` failed, `2` usage error
  ([0020](docs/adr/0020-check-the-endpoint.md)).
- `--model-timeout` on `kausa run` and `kausa check-endpoint`: a call's total timeout, 10 to 600
  seconds, 120 by default; the `started` line shows it as `model_timeout_seconds`.
- The chart's `helm test`: a Pod `<fullname>-endpoint-check` that runs `kausa check-endpoint` with
  no ServiceAccount token, created only by `helm test`, and, with `networkPolicy.enabled`, its own
  NetworkPolicy with egress to DNS and the endpoint only. The values `model.timeoutSeconds` (120)
  and `endpointCheck.enabled` (true).
- Two gauges: `kausa_endpoint_state{state}` (`closed`, `half_open`, `open`) and
  `kausa_model_last_success_timestamp_seconds`; the skip reason `endpoint_unavailable`; the log
  lines `endpoint unavailable` and `endpoint available`; `cause` and `queued` on `explanation
  retried`, and `cause` and `status` on `endpoint error`.

### Changed

- The hourly call budget refills at a steady rate instead of starting full: each start (and, with
  `--leader-lease`, each takeover) allows 3 calls at once, then one every `3600/N` seconds up to
  `N` (`--max-calls-per-hour`), and the 60-minute and daily caps still hold. A restart no longer
  brings a full hour's budget. `--max-calls-per-day`'s help says it counts per process
  ([0021](docs/adr/0021-refill-the-call-budget.md)).
- While the budget waits, incidents wait in the queue instead of being skipped, and a repeat with a
  cached explanation passes them. When 16 are waiting, the oldest is skipped with an
  `ExplanationSkipped` Event (reason `budget`). A spent daily cap and a cap of 0 still skip at once.
- A budget skip no longer reads the container log, so it may come where `context_too_large` came
  before, and delayed retries no longer take the queue past 16.
- A failed model call gets a cause and a class ([Causes](docs/reference/command-line.md#causes)):
  a quota `429` (`insufficient_quota`) and a `402` are no longer retried, nor a redirect, a refused
  key, a missing model or an answer that is not a chat completion; `Retry-After` is honoured on
  `429` and `503`, in seconds, clamped to 30-600 ([0020](docs/adr/0020-check-the-endpoint.md)).
- After 3 transient failures in a row or 1 configuration error, kausa stops calling the endpoint
  and incidents wait, one probing per hold (60 seconds, doubling up to 30 minutes); with 16
  waiting, the oldest is skipped as `endpoint_unavailable`. A transient first failure that opens
  the breaker sends its incident to the back of the queue.
- A `413` is skipped as `context_too_large`, with the note `the endpoint refused the request as too
  large for the model: lower --max-input-tokens`.
- A chat completion with no content (a refusal, a filter, out of output tokens) is still skipped
  at once with reason `endpoint`, now with a fixed note that names the kind.
- A watch error that repeats with the same text is logged at most once every 5 minutes, with a
  `folded` field counting the repeats in between; `kausa_watch_errors_total` still counts every
  one ([#7](https://github.com/fedeam/kausa/issues/7)).
- `R014` also redacts the query and fragment of an HTTP request line without a scheme
  (`GET /orders?page=2 HTTP/1.1` becomes `GET /orders?<redacted:query> HTTP/1.1`), with an
  uppercase method; requests send less.
- The help text of `kausa_redactions_total` says that it counts the requests' redactions; its
  series do not change.
- After the upgrade, an Explanation may start a new entry once: a repeated incident whose
  explanation now redacts differently goes in front instead of counting on the old entry, which
  may push the oldest of the five out. Entries written before keep their text until
  `--history-days` ages them out.
- The Pod, Event and Deployment watches list in pages of 50 objects instead of 500, which bounds
  the memory a page of large objects takes while it is decoded, for about ten times the list
  requests ([List pages](docs/research/2026-11-02-list-pages.md)).
- `D004` is named `container cannot be created or started` and `D011` `workload cannot create pods
  or jobs`; the names are in requests to the model, in Events and in the `detection.name` of
  notifications.
- A container waiting in `CrashLoopBackOff` whose last termination is `StartError` is `D004`, not
  `D001`, as `OOMKilled` is `D003`: one incident with no log read, since the container never ran.
- `D011` about a CronJob itself is an incident at once instead of waiting for the settle window,
  which the CronJob controller's events would never pass. A Job of a CronJob whose Pods are
  refused still waits for the window.

### Fixed

- An incident whose Pods keep being replaced while it is pending no longer keeps a list of Pod
  names that grows without limit: kausa keeps the first 100 names in sorted order, plus the Pods
  failing when it settles ([#6](https://github.com/fedeam/kausa/issues/6)).

## [0.4.1] - 2026-10-30

A fix for Slack notifications, which in v0.4.0 could notify a whole channel, and a stricter
redaction of URLs. Nothing to change in values or permissions.

### Added

- Redaction rule `R014`: the query and fragment of any URL with a scheme in event text or a log
  become `?<redacted:query>` and `#<redacted:fragment>`, whatever they hold. It closes the gap
  where a kubelet `Unhealthy` note quoted a probe URL's query as written
  ([#48](https://github.com/fedeam/kausa/issues/48)).

### Fixed

- A `slack` notification could notify a whole channel: a real post on 2026-10-30 showed that Slack
  turns a plain `@here` or `@channel` in the text into a mention, although kausa sends
  `"parse": "none"` and no `link_names`. The Slack text now carries a word joiner (U+2060) after
  every `@`, as the Telegram text does ([#45](https://github.com/fedeam/kausa/issues/45)).

## [0.4.0] - 2026-10-30

The detection rules `D011` to `D014` (`D014`, a stuck rollout, from a new Deployment watch), a
richer context for the model (0017) and an optional standby replica, off by default (0019).
Upgrade the image and the chart together, since the new image needs the chart's `deployments` rule
to become Ready; the fingerprints of `D001` and `D003` change once, and the default memory limit is
`192Mi`.

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
- Detection rule `D014`, rollout does not progress: a Deployment whose `Progressing` condition is
  `False` with reason `ProgressDeadlineExceeded`, not paused and observed at its current
  generation, an incident about the Deployment, with one fingerprint per revision. It yields to an
  explained incident of the same Deployment (a crash loop, a refused Pod), so a stall is explained
  once, and the request carries only the Deployment's replica counts and deadlines (`rollout`)
  ([0018. Explain a Deployment past its progress deadline (`D014`) from a Deployment watch that is
  on by default, and add nothing for StatefulSets or DaemonSets](docs/adr/0018-stuck-rollouts.md)).
- `kausa run --no-rollouts` turns the Deployment watch, and `D014`, off; the `started` line gains
  `rollouts`, and the log `deployment watch error`.
- The chart's `rollouts` value, `true` by default (a missing value means on): `false` passes
  `--no-rollouts` and removes the `deployments` rule.
- An optional standby replica: the chart's `replicaCount` value, `1` by default (a missing value
  or `null` means `1`; anything but `1` or `2` is refused). With `2`, two Pods run with
  `Recreate`, and the chart adds the Lease `<fullname>-leader` (`spec: {}`), a Role and
  RoleBinding of that name with `get` and `patch` on that one Lease in the release namespace (the
  same in both scopes), a PodDisruptionBudget (`maxUnavailable: 1`, `unhealthyPodEvictionPolicy:
  AlwaysAllow`), a preferred pod anti-affinity while `affinity` is empty, and a paragraph in
  `NOTES.txt`. With `1` the chart renders as before, byte for byte ([0019. Offer an optional
  standby replica that takes over through a chart-created Lease, elected by kausa's own
  code](docs/adr/0019-high-availability.md)).
- `kausa run --leader-lease <NAMESPACE>/<NAME>`: act only while holding that Lease, as a standby
  otherwise, in every mode; it needs `POD_NAME`. The `started` line gains `leader_lease`, and the
  log `standby`, `leading`, `leadership lost`, `lease released`, `lease release failed` and
  `lease error`.
- The gauge `kausa_leader` (`1` while the process acts, which is always without
  `--leader-lease`; `0` as a standby), and the counters `kausa_leadership_changes_total{to}` and
  `kausa_lease_errors_total{call}`.
- `/readyz` answers `200` with `standby` on a replica that waits for the Lease.

### Changed

- Requests to the model carry each container's resource requests, each probe's handler, port and
  success threshold, the probe path without its query and fragment for `D001` and `D006`, the image
  pull policy and the number of pull secrets for `D002`, and an unschedulable Pod's node selector,
  affinity and toleration keys (no values but `kubernetes.io/os` and `kubernetes.io/arch`) for
  `D005`; `--dry-run` shows them. Resource quantities are checked against the Kubernetes quantity
  grammar. The prompt version is `2`, so Explanations store `promptVersion` `"2"`. No permission,
  flag or chart value changes. A tight `--max-input-tokens` loses log bytes earlier on `D001`,
  `D003` and `D008` ([0017. Send the model resource requests, probe handlers, image pull settings
  and an unschedulable Pod's scheduling keys, reduced and
  redacted](docs/adr/0017-richer-context-for-the-model.md)).
- `R003` redacts more names in event messages: the quoted name after `pod`, `container` and
  `StorageClass` (every name of a list), after a resource qualified by its API group
  (`deployments.apps "<name>"`), and the quota's name in `exceeded quota: <name>`. These run as a
  second pass after the pattern of 0.3.0, which is unchanged, so they only add redactions.
- The request names a PersistentVolumeClaim or a HorizontalPodAutoscaler that an incident is about
  by its kind (`persistentvolumeclaim-1`, `horizontalpodautoscaler-1`), not as `Object`.
- kausa reads Deployments: the chart grants `list` and `watch` on `deployments` (`apps`) in the
  ClusterRole and in every Role, while `rollouts` is on. No `get`, no write, nothing on
  StatefulSets or DaemonSets.
- kausa runs three watches, Pods, Warning events and Deployments, on the whole cluster or in each
  listed namespace, and `/readyz` answers `200` once all of them have listed (3, or 3 × N; 2 and
  2 × N with `--no-rollouts`). `kausa_watch_errors_total` gains `watch="deployments"`, and an
  ended Deployment watch stops kausa with `the deployment watch ended`.
- Upgrade the chart and the image together: the new image without the `deployments` rule (the
  previous chart's role, RBAC made outside the chart, a read-only proxy) explains everything else
  but is never Ready; add the rule or use `--no-rollouts`.
- The chart's default memory limit is `192Mi` (was `128Mi`): with 10,000 unschedulable Pods at the
  largest scheduling record kausa peaked at 138.8 MiB ([Richer context,
  measured](docs/research/2026-10-29-richer-context-measured.md)).
- An Event write (its create, and on a repeat its list and series patch) gives up after 10 seconds
  and logs `event write failed` with `no answer within 10 seconds`; it had no timeout. Each list
  and delete of the history sweep is bounded to 10 seconds within the sweep's 30. Both hold with
  or without `--leader-lease` ([0019. Offer an optional standby replica that takes over through a
  chart-created Lease, elected by kausa's own code](docs/adr/0019-high-availability.md)).

### Fixed

- The fingerprint of a crash loop (`D001`) or an out-of-memory kill (`D003`) no longer depends on
  whether kausa saw the container waiting in `CrashLoopBackOff` or terminated between restarts:
  it hashes the container's name instead of the message. The same failure kept its Explanation and
  Event names only some of the time, after a restart of kausa for example. These fingerprints
  change once on upgrade, so a failure that was already explained gets one new Explanation.

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

[unreleased]: https://github.com/fedeam/kausa/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/fedeam/kausa/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/fedeam/kausa/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/fedeam/kausa/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/fedeam/kausa/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/fedeam/kausa/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/fedeam/kausa/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/fedeam/kausa/releases/tag/v0.1.0
