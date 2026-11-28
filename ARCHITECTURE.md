# ARCHITECTURE.md — kausa

A map of the code for whoever changes it, person or coding agent: where each part lives, how data
flows, the invariants no change may break, and the path each kind of change takes. The design
and its reasons are in [Architecture](docs/explanation/architecture.md) and the decisions in
[`docs/adr/`](docs/adr/); how to work in this repository is in [AGENTS.md](AGENTS.md).

## The flow

```text
API server ──watch──▶ run.rs ──▶ record.rs ──▶ detect.rs ──▶ incident.rs ──▶ (settled incident)
  Pods, Warning         loop      small records   rules         signals, grouping,      │
  Events, Deployments             (objects dropped)              settle window,          │
                                                                 fingerprint.rs          ▼
                          owner.rs ◀── metadata get of ReplicaSets and Jobs        explain.rs queue
                                                                                        │
     context.rs ◀── allowlisted fields ── redact.rs (placeholders, patterns) ◀──────────┘
          │
          ▼
     prompt.rs ──▶ breaker.rs ──▶ budget.rs ──▶ model.rs ──▶ prompt::validate ──▶ redact::answer
     (request,     (holds calls   (calls per    (one POST,    (schema, caps,       (R003–R014 on
     untrusted     while the      hour/day,     HTTPS, no     cleaning)            the answer)
     wrapper)      endpoint       dedup cache)  redirects;                              │
                   fails)                       model/pure.rs                           ▼
                                                names the cause)                  restore names
                                                                                          │
                                                                                          ▼
                                                                                          prompt::withhold_all
                                                                                          (W001–W006 on every
                                                                                          version: the notice,
                                                                                          low confidence)
                                                                                          │
                                                                                          ▼
                                                                                          events.rs (an Event
                                                                                          on the owner)
                                                                                          │
                                                                                          ▼
                                                    history.rs (with --history: the Explanation
                                                    in the incident's namespace; the sweep takes
                                                    the explanation slot at start and hourly)
                                                                                          │
                                                                                          ▼
                   notify.rs queue (with --notify-url-file: a new explanation, its names restored
                   by redact::restore_only for the namespace and the owner only) ──▶ run.rs sends
                   it beside the watches, one at a time ──▶ notification.rs body ──▶ the webhook

The fact path runs beside the explanation path, and neither waits for the other, so an
incident's `Detected` and `Explained` Events, and its two messages, can come in either order:

  (settled incident) ──▶ fact.rs statement ──▶ report.rs fact queue (1024) ──▶ fact slot:
      the `Detected` Event (events.rs) ──▶ notify.rs queue: the `detected` message ──▶ the webhook
  with or without --endpoint; the `incident` line carries the same statement

With --leader-lease (two replicas): run.rs ◀── leader.rs keeper (get / merge patch of one Lease
every 5 s; pure decisions in leader/pure.rs) ── only the holder runs the loop above, and every
write and webhook attempt first checks that its term (sent + 20 s) still holds
```

Everything else is around it: `main.rs` parses the flags, `log.rs` writes kausa's own log,
`metrics.rs` serves the counters, gauges and histogram and the health endpoints. `kausa
check-endpoint` (`check.rs`) is apart from this flow: a fabricated incident through `context.rs`,
`prompt.rs`, `model.rs` and `prompt::validate`, on demand, with no Kubernetes client, breaker or
budget.

## Codemap

| File | What it does | Change it when |
|---|---|---|
| `src/main.rs` | The two commands, flags, usage errors (`--namespace` checked before any connection, at most `MAX_NAMESPACES`), the six model flags both commands share, parsed by one helper (`ModelFlags`), `--incidents` and `--incident-days` checked against `--dry-run` (`check_incidents`) and turned into the incidents' settings with the gate's window of `--dedup-hours` (`incident_settings`), the settings passed to `run` | A flag is added or changed |
| `src/check.rs` | `kausa check-endpoint`: the compiled-in, fabricated `D001` incident, its request (pinned by `tests/golden/check-endpoint-request-*.json`), the one call and the one `endpoint check` line with its step booleans; no Kubernetes client, no budget, no retry | The check's request or line changes (the request needs an ADR, as anything sent to the model) |
| `src/run.rs` | `run()` keeps what lives across terms (client, signals, metrics listener and connections, the `Term`, and `Shared`: `Budget`, `Dedup`, the writers and the notification queue) and, with `--leader-lease`, alternates a standby phase and a leading phase, stepping down in place on a lost Lease and releasing it on a stop; `lead()` is one term of the controller loop: one Pod watch, one event watch and (unless `--no-rollouts`) one Deployment watch for the whole cluster, or three per `--namespace`, merged per kind and tagged with their watch; relists, `403` drops and a new watcher after a `403` or an error inside an open watch (`Restarting`) per watch; repeated watch errors folded in the log (`WatchErrors`); lists in pages of `LIST_PAGE_SIZE` (50); readiness once every watch has listed; the tick, the explanation queue and the admission of the explanation slot (`allowance`, `admit`, `enqueue`: the breaker first, then the budget; while either holds, jobs wait, a cached repeat passes them, and the oldest of a full queue of 16 is skipped with the reason of the gate that held it), what `explain::run` returns (`Next`: a delayed retry, a requeue at the back through `enqueue`), the `explanations waiting` line, the `kausa_endpoint_state` gauge at each pass, relists and restarts counted per watch, the term's record and owner cache gauges at each tick (`show_held`, `0` at start and on a step-down), the build info and the counters with fixed labels at `0` at start (`show_start`, `show_zero_counters`, by mode; the four counters of the Incidents only with `--incidents`, so that the start exposition with the defaults stays `tests/golden/metrics-start.prom`), the budget's term start in `serve`, the notification slot beside it (one send at a time, which a duplicate skips), the reporter, built when any of the `Detected` Events, the webhook or the Incidents is on, and the incident sweep's slot beside the fact slot (at each term start and then hourly, only with `--incidents`), the metrics listener, owner lookups with a cache | The loop's scheduling or what it watches changes |
| `src/leader.rs` | With `--leader-lease`: the calls on the one Lease (`get`, and a JSON merge patch of `spec` with a `resourceVersion` precondition, under a 5-second timeout), the `Term` the write guard reads, the standby (read every 5 s, take a free or stale Lease), the keeper (renew every 5 s, step down on another holder or at its own deadline), the release, and the rate-limited `lease error` lines | How the replicas elect their leader changes (0019) |
| `src/leader/pure.rs` | The pure part of the election: the timings and `WRITE_BOUND` (with the compile-time check `RENEW_DEADLINE + WRITE_BOUND <= LEASE_DURATION`), what kausa reads from an untrusted Lease (version and holder), the take-or-wait decision on the monotonic clock, `leaseTransitions`, and the exact acquire, renew and release bodies | The election's rules, timings or bodies change |
| `src/record.rs` | Reduces a Pod, an Event or a Deployment to the few fields kausa keeps; the object is dropped at once (a Deployment's record is kept only while `D014` matches) | A rule needs a field that is not recorded |
| `src/detect.rs` | The detection rules `D001`–`D014`: pure functions of a record | A detection rule is added or changed |
| `src/incident.rs` | Signals, grouping by top-level owner, the settle window, and the yield of `D014` to an explained incident of the same Deployment | Grouping or settling changes |
| `src/fingerprint.rs` | The incident fingerprint (dedup key, Event name, notification field), never sent to the model | What makes two incidents "the same" changes |
| `src/owner.rs` | ReplicaSet → Deployment and Job → CronJob, metadata only; the rule that a name to look up is a DNS-1123 subdomain and its namespace a DNS-1123 label, and the two predicates the crate shares; `replica_set`, the UID and revision of a sample's ReplicaSet through the same cache, read only by the Incident's create, only with `--incidents` ([0028](docs/adr/0028-incident-resource-and-remediation-report.md), point 3) | A new owner hop (needs an ADR: a new permission) |
| `src/fact.rs` | The pure, fuzzed fact statement: one line built from an incident and `pods_total`, with the reasons list compiled in, the DNS-label check of the container, the facts object of a notification, `MAX_STATEMENT_BYTES` and `LONGEST_STATEMENT_BYTES`; the examples of [Detection and redaction rules](docs/reference/rules.md#fact-statements) are tied by a test | What a statement may hold changes (needs an ADR, invariant 12) |
| `src/report.rs` | The fact path: the fact queue (1024, drops its oldest, `fact dropped`, `kausa_facts_dropped_total`; the queue itself is pure) and the fact slot (`Reporter`), which writes one `Detected` Event at a time through `events.rs` (unless `--no-events`) and then queues the `detected` message through `notify.rs`, then, only with `--incidents`, creates the incident's Incident through `incidents.rs` (with `--no-events` and no webhook too); the fact's data for the Incident (`IncidentData`, the sample's UIDs or the Event's `regarding`), kept only with `--incidents`, and its bytes; it never waits for the model, the budget, the breaker or an explanation | The fact path changes |
| `src/explain.rs` | One incident to one explanation: the breaker's and the budget's refusal (`refusal`) before the log is read, log tail, fit to the token cap, dry-run or call, each call counted and timed by outcome (`called`), the call recorded into the breaker (`Shared.breaker`) before the term check with its `endpoint unavailable` / `endpoint available` lines and the last-success gauge, the job's outcome by cause (`Next`: done, retry, requeue), validation, Event; the term guard (`Shared.term`) before the budget take, after the model's answer and before each write; after the answer pass and both restores, `prompt::withhold_all` on the placeholder, restored and notification versions before the dedup entry, the log line, the Event, the history and the notification, with `kausa_answers_withheld_total` and the `withheld` and `model_confidence` fields of the fresh `explanation` line | The explanation pipeline changes |
| `src/context.rs` | The context sent to the model: allowlisted fields only (`R001`), all names and text redacted | Anything sent to the model changes (needs an ADR) |
| `src/redact.rs` | The redaction rules `R002`–`R014`; the answer pass (`Redactor::answer`), which runs the pattern rules of `ANSWER_RULES` (all but `R002`) on the model's answer and counts them apart; the mapping back of placeholders, and `restore_only`, which maps back only the names a notification may carry | A redaction rule is added or changed (a pattern rule also joins `ANSWER_RULES`) |
| `src/prompt.rs` | The request (one of three fixed system messages, chosen by detection, the untrusted wrapper, the answer schema), the validation of the answer, and `map_clean`, which passes each field of an answer through a function (the answer pass, the restores) and cleans and caps it again; the withheld command shapes (`SHAPES`, `W001`–`W006`, published verbatim in [rules.md](docs/reference/rules.md#withheld-command-shapes) and tied by a test), the folded copy and its candidates, `shapes`, `withhold_all` and `withhold`, the two fixed texts `WITHHELD` and `WITHHELD_FIELD`, and `Explanation::withheld` | The request or the answer's format changes, or a withheld shape (needs an ADR, a corpus case, a near miss and a replay) |
| `src/budget.rs` | The call budget: a refill bucket (a start burst of `START_BURST`, 3, then one call every `3600/N` seconds, capacity `N`) under the 60-minute and 24-hour windows, `check` (an `Allowance`: `Now`, `Wait { until }` or `Never`) that changes nothing, `take` (the authority), `refund`, `start_term` (cuts the credit to the burst) and `remaining` for the gauges; the deduplication cache (with each entry's notification text and whether the webhook acknowledged it, and `is_fresh`) | Cost controls change |
| `src/model.rs` | The client of the one OpenAI-compatible endpoint: `--model-timeout`, the reqwest errors mapped to `connection` and `timeout`, `Retry-After` read before the body, and a `CallError` with its cause, status and delay | The HTTP call changes |
| `src/model/pure.rs` | The pure, fuzzed reading of an endpoint's answer: `Cause` and its class and hint, `classify` (status, then `error.code` / `error.type` against `QUOTA_CODES` and `TOO_LARGE_CODES`), `completion` (a chat completion, `not_an_answer` or `no_content` of a kind), `retry_after` (clamped to 30-600 s) | A cause, a code list or the reading of an answer changes (a code joins a list only with a captured body as a fixture) |
| `src/breaker.rs` | The pure breaker in front of the budget: `OPEN_AFTER` (3), `FIRST_HOLD` (60 s), `MAX_HOLD` (1800 s), `state(now)` and `record(call, now)`, which returns the transition the log reports; no clock of its own | How kausa holds calls while the endpoint fails changes (needs an ADR) |
| `src/events.rs` | The `Detected` Event (`Warning`, action `Detect`, the statement as its note, named `kausa.detected.<fingerprint>.<8 hex>`), the `Explained` Event (still `Normal`), whose note is the mark `Model's explanation, not verified: ` and the model's text, after kausa's notice when the answer was withheld, and the `ExplanationSkipped` Event; a repeat patches only its series; each call checks the term first, and the whole write ends within `WRITE_BOUND` (10 s) | The output changes |
| `src/notification.rs` | The pure, fuzzed bodies of a notification (`detected`, `explained` and `capped`, schema `v2`): `json`, the shared plain text, `slack` (escaped) and `telegram` (with word joiners), and, only in tests until the Remediation watch lands ([0028](docs/adr/0028-incident-resource-and-remediation-report.md)), the `remediation` body and text in each format, built from a `remediation::Proposal`; with a withheld answer's notice on the line after the header (`WITHHELD`, a copy of `prompt::WITHHELD` tied by a test in `src/notify.rs`, since this file has no `crate::` import), the 4,000-unit cut, the Telegram chat id | What a notification carries changes (needs an ADR, and [Data sent in notifications](docs/reference/data-sent-in-notifications.md)) |
| `src/notify.rs` | The webhook transport, with or without an endpoint, and the bound of `detected` messages (once per fingerprint per `--dedup-hours`, `--notify-max-per-hour`, the `capped` message): the URL checks, the `reqwest` client (no proxy, no redirects), one send with its retry (each attempt checks the term first), the queue of 16 and the dequeue rule, the `notification *` log lines and counters; only in tests until the Remediation watch lands ([0028](docs/adr/0028-incident-resource-and-remediation-report.md)), the lower class of `remediation` messages (`Low`, `enqueue_low`, `take_low`, `deliver_low`): it shares the 16 entries, is dropped first when they are full and taken last, has its own cap on the same `--notify-max-per-hour`, never owes the `capped` message, logs its `remediation dropped` and `remediation capped` lines at most once an hour each, and keeps its outcomes, not rendered as metrics yet | How notifications are sent or queued changes |
| `src/history.rs` | With `--history`: the writer of the Explanation of an explained incident (`list` by name, then `create` or a merge patch of `status`, under one 10-second timeout) and the sweep (age, then `MAX_OBJECTS`, with `resourceVersion` preconditions, each call within 10 s), both checking the term before each call, over `Api<DynamicObject>` | What kausa keeps of an explanation, or its retention, changes |
| `src/history/pure.rs` | The pure, fuzzed part of the history: the name, the create and patch bodies, the reading of a stored (untrusted) object, the identity check, the merge and the sweep's plan, which with `SweepPlan::incidents` also plans the sweep of Incidents by `metadata.creationTimestamp` alone, each delete with the UID | The Explanation's content, the merge or the sweep's plan changes |
| `src/incident_resource.rs` | Used only with `--incidents` ([0028](docs/adr/0028-incident-resource-and-remediation-report.md)): the pure Incident's `spec`, built from a fact by `Draft::of`, with the ReplicaSet read it needs and the revision it gives (`Draft::after_read`), its name `kausa.<fingerprint>.<firstSeenUnix>`, and `Spec::check`, which refuses what the schema would, naming a `Field` and never a value; the schema's patterns, lengths, enums and CEL rules as constants, tied to `charts/kausa/files/incident-schema.json` by its tests | The Incident's fields, limits or rules change (needs an ADR) |
| `src/incident_resource/table.rs` | Used only with `--incidents` ([0028](docs/adr/0028-incident-resource-and-remediation-report.md)): the table of created Incidents (`CAPACITY`, 500 fixed slots; an entry made only with the UID the server gave; none dropped before `KEEP`, 6 hours; the oldest past it dropped for room), the gate (`admit`: `Create`, `Duplicate` inside `--dedup-hours`, or `Refused` with `table-full` or `starting`), the lookup by namespace, name and UID, the mark of a reported Incident, and the `incident refused` limiter, once an hour per reason; every instant is injected | The gate, the table's bounds or the refusal reasons change (needs an ADR) |
| `src/incidents.rs` | Only with `--incidents` ([0028](docs/adr/0028-incident-resource-and-remediation-report.md), point 1): the create of an Incident in the fact slot, after the `Detected` Event and the `detected` message: the gate, the `starting` and `table-full` refusals, the schema check before the read, the ReplicaSet's read, then one `create` within `WRITE_BOUND` (10 s) while the term holds, its answer read under `CREATE_ANSWER_BYTES` (64 KiB) and kept only as the table's UID; and the sweep of every Incident (point 4: `list` in pages of `SWEEP_PAGE`, `delete` with UID and `resourceVersion` preconditions, by age past `--incident-days` and beyond `MAX_OBJECTS`, within `SWEEP_TIME`, each call within `WRITE_BOUND` while the term holds; a `404` or `409` ignored), whose end, but for a cut term, lets creates through; refusals, deletions and errors counted in the four counters of `metrics.rs`'s `INCIDENT_COUNTERS` (`REFUSED`, `WRITE_ERRORS`, `DELETED`, `SWEEP_ERRORS`) and logged; nothing retried | How kausa creates or sweeps Incidents changes (needs an ADR) |
| `src/remediation.rs` | Only in tests until the Remediation watch lands ([0028](docs/adr/0028-incident-resource-and-remediation-report.md)): the pure, fuzzed parse and checks of a kurita Remediation, which is untrusted: `parse` reads only the fields it needs, each against the pattern and the length of kurita's schema, and `check` runs the eight checks in order against the table of Incidents and gives `Report`, `Nothing` (the action `none`) or `Ignored` with the first reason that fails (`invalid`, `incident-unknown`, `unknown-action`, `target-mismatch`, `time-out-of-range` or `duplicate`); `ALLOWED`, the actions allowed per detection; `Proposal`, what the message carries, built only from the table's entry, the `Action` and the checked time; the `remediation ignored` limiter, once a minute per reason (`IGNORED_LOG_PERIOD`); every instant is injected | A check, an action, a reason or what the message carries changes (needs an ADR, and [Data sent in notifications](docs/reference/data-sent-in-notifications.md)) |
| `src/metrics.rs` | The 25 counters (`COUNTERS`; among them `kausa_facts_dropped_total` and `kausa_fact_reasons_omitted_total` for the fact path, `kausa_events_written_total{reason="Detected"}` and `kausa_notifications_total{outcome="capped"}`; among them `kausa_redactions_total` for the requests, `kausa_answer_redactions_total` for the answers, `kausa_answers_withheld_total` for the answers kausa withheld (last), and `kausa_watch_relists_total` and `kausa_watch_restarts_total` per watch), a counter's unlabelled `0` line until a series exists (`run.rs` starts the counters with fixed labels at `0` per value), the four counters of the Incidents (`INCIDENT_COUNTERS`: `kausa_incidents_refused_total`, `kausa_incident_write_errors_total`, `kausa_incidents_deleted_total` and `kausa_incident_sweep_errors_total`), rendered after the others and only once a series exists, which `run.rs` sets only with `--incidents`, the nine gauges (`GAUGES`: `kausa_leader`, always rendered, and `kausa_budget_calls_remaining`, `kausa_explanation_queue_length`, `kausa_endpoint_state`, `kausa_model_last_success_timestamp_seconds`, `kausa_build_info`, `kausa_records`, `kausa_owner_cache_entries` and `kausa_dedup_cache_entries`, through `set_gauge`, rendered only once set), the histogram `kausa_model_call_duration_seconds` (`HISTOGRAMS`, `CALL_DURATION_BUCKETS`, `observe`, rendered only once observed), `/metrics`, `/healthz`, `/readyz` (`starting`, `ready`, or `standby` for a replica that does not hold the Lease); its tests pin the whole exposition to `tests/golden/metrics.prom` and tie the metrics the alerting rules read, and their sections in Alerts, to the lists | A counter, gauge or histogram is added |
| `src/log.rs` | One JSON object per line, through a serializer | Only to keep it safe |
| `charts/kausa/` | The Helm chart: RBAC by `rbac.scope` (`templates/rbac.yaml`: a ClusterRole, or a Role per listed namespace, both from `kausa.rules`, which with `incidents.enabled` and no `dryRun` (`kausa.writesIncidents`) adds `create`, `list` and `delete` on `incidents` last), the refusals of `kausa.validate`, Deployment, Service, NetworkPolicy (`templates/networkpolicy.yaml`: kausa's, and a second, `<fullname>-endpoint-check`, with egress to DNS and the endpoint only, for the test Pod) | Install, permissions or a flag change |
| `charts/kausa/templates/tests/` | `endpoint-check.yaml`: the `helm test` Pod that runs `kausa check-endpoint` (`before-hook-creation`, no ServiceAccount token, the namespace's `default` identity, the API-key Secret only, labels that no selector of kausa's matches), rendered by `kausa.endpointCheck` | The endpoint check's Pod changes |
| `charts/kausa/templates/leader-election.yaml` | Only with `replicaCount: 2`: the Lease `<fullname>-leader` (`spec: {}`; Helm owns the object, kausa its `spec`), and the Role and RoleBinding of the same name with `get` and `patch` on that Lease alone, the same in both scopes, outside `kausa.rules` | The standby's Lease or its one permission changes (needs an ADR) |
| `charts/kausa/templates/poddisruptionbudget.yaml` | Only with `replicaCount: 2`: `maxUnavailable: 1` and `unhealthyPodEvictionPolicy: AlwaysAllow` | How a drain treats the two replicas changes |
| `charts/kausa/files/explanation-schema.json` | The Explanation's `openAPIV3Schema`, with every cap; read by the CRD template and by the schema test in `src/history/pure.rs` | The Explanation's fields or limits change (a CHANGELOG line) |
| `charts/kausa/files/incident-schema.json` | The Incident's `openAPIV3Schema`, hand-written: every string an enum or a pattern with a `maxLength`, `spec` required and immutable, an empty `status`, the CEL rules of [0028](docs/adr/0028-incident-resource-and-remediation-report.md); read by `templates/incident-crd.yaml` and by the schema tests in `src/incident_resource.rs` | The Incident's fields, limits or rules change (needs an ADR) |
| `charts/kausa/templates/explanation-crd.yaml`, `explanation-reader.yaml` | The CustomResourceDefinition `explanations.kausa.fedeam.com` and the reader ClusterRole aggregated to `view`, only while the release writes history with `history.installCRD` (the reader also with `history.readers.aggregate`) | The CRD's columns or its readers change |
| `charts/kausa/templates/incident-crd.yaml`, `incident-reader.yaml` | The CustomResourceDefinition `incidents.kausa.fedeam.com` (the schema file's bytes, an empty `status` subresource, no `helm.sh/resource-policy: keep`) and the reader ClusterRole `<fullname>-incident-reader` (`get`, `list`, `watch`), aggregated to `view` only, while the release creates Incidents with `incidents.installCRD` (the reader also with `incidents.readers.aggregate`) | The CRD or its readers change (needs an ADR) |
| `Dockerfile` | The `ci` target (every check) and the `runtime` image (built with cargo auditable) | A check, a pinned tool or the image changes |
| `deploy/prometheus/` | `kausa-alerts.yaml`: the Prometheus alerting rules users load beside the chart, on kausa's own metrics only, `severity: warning` alone, each `runbook_url` on its section of [Alerts](docs/reference/alerts.md); checked by `promtool check rules` in the `ci` target | An alert is added or changed, with its unit test and Alerts |
| `tests/prometheus/` | `kausa-alerts.test.yaml`: the `promtool test rules` unit tests of every alert, each threshold, window and `for` one minute either side of its boundary | An alert is added or changed |
| `tests/cli.rs` | Command-line behaviour and exit codes, `kausa check-endpoint` against a local fake server included | A flag or exit code changes |
| `tests/chart/render.sh` | Chart checks with helm alone: both scopes, the rules against the v0.1.0 chart, each notification format, `NOTES.txt`, every refusal, the Artifact Hub metadata and `README.md` of the chart, the incidents on and off (rules, CRD, reader role, arguments, refusals), the default render of each scope byte for byte against `tests/golden/chart-default-cluster.yaml` and `chart-default-namespaced.yaml`, and the `artifacthub.io/images` checks taken from the `Dockerfile` and `release.yml` | The chart changes |
| `tests/e2e/` | The kind test, once per `rbac.scope` (`KAUSA_E2E_SCOPE`) and once more namespaced with a standby (`KAUSA_E2E_REPLICAS=2`: graceful, frozen and fail-closed handovers): chart, Events, what leaves the cluster (to the fake model and the fake webhook), a withheld answer in every sink (the fake model answers the incident whose log carries `--e2e-withhold-marker` with a destructive step), the ClusterRole or the Roles and what they grant, an unlisted and a refusing namespace, metrics, `helm test` (a pass, and `not_found` on a missing model) and, in the cluster scope, the breaker against a failing fake endpoint; with one replica, the Incidents turned on and off again by an upgrade (the grant on and off, one Incident per failing workload against its log line and `Detected` Event, the API server's refusals, `/status`, readers through `view`, `edit` and `admin`, the sweep at start, no Remediation). `tests/e2e/upgrade.sh`: the latest release, pinned in `tests/e2e/upgrade-from.txt` (version, chart SHA-256, image digest), installed with two replicas and upgraded with `helm upgrade --reuse-values` to the checkout's chart and image, once per `rbac.scope`: rollout, values, the Lease split and an explanation after the upgrade. `tests/e2e/lib.sh`: the helpers both scripts source (constants, `k` and `h`, cleanup, the fake endpoint, the checks) | The chart or what kausa writes changes; `tests/e2e/upgrade-from.txt` after each release |
| `fuzz/`, `.clusterfuzzlite/` | Fuzz targets for redaction (with the answer pass, `restore` and `restore_only`), answer validation (with `withhold` and `withhold_all`: no shape survives), the history's merge, the notification bodies (`notification`), the endpoint's answers (`endpoint_error`, `src/model/pure.rs`), the fact statement (`fact`) and the parse and checks of a Remediation (`remediation`), run by ClusterFuzzLite; seed corpora for `redact`, `validate` and `remediation` in `fuzz/seeds/<target>/`, zipped by `.clusterfuzzlite/build.sh` | A parser of untrusted text changes, or a new one is added |
| `tests/soak/` | The soak, kept out of CI ([Run the soak](docs/how-to/run-the-soak.md)): its values and fake endpoint, the scripts that install the pinned release in the cluster of an explicit kube context, open the breaker once a day and uninstall it, the sampler (`sample.py`), its launcher on macOS (`launch.py`), the coverage check while the soak runs (`coverage.py`), and the report with the verdicts of the decision rule (`summarize.py`, with `--self-test`) | A chart value the soak sets changes (not a new metric: the sampler records every `kausa_` series), or the release pin's file format |
| `tests/bench/` | The benchmarks, kept out of CI: the seeded failures (one or more per detection), their known causes with a fixed rubric (`known-causes.json`), the scorer that replays dry-run requests against an endpoint and grades the answers (`score.py`, no model as judge), the token-counting proxy and the scale scripts | A benchmark is repeated, or a detection is added or changed (its seeded failure and known cause) |

Unit tests live next to the code, in each file's `tests` module.

## Invariants

A change that breaks one of these is wrong even if every test passes. Changing one needs an
accepted ADR, and most should never change.

1. **kausa never reads Secrets or ConfigMaps**, in any verb, and asks for no permission beyond
   [Permissions](docs/reference/permissions.md) ([0009](docs/adr/0009-least-privilege-rbac.md)).
   In namespaced scope it is granted those only in the listed namespaces, and nothing
   cluster-scoped; the watch and the grant come from one list
   ([0013](docs/adr/0013-namespaced-install-mode.md)). With `--leader-lease` it is also granted
   `get` and `patch` on its one Lease in its own namespace, in either scope
   ([0019](docs/adr/0019-high-availability.md)).
2. **The only writes are Events** (`create`, and a `patch` of the series of an Event kausa wrote)
   and, with `--history`, Explanations (`create`, a `patch` of the `status` and `resourceVersion`
   of one that carries kausa's label and the incident's `spec`, and, for retention, a `delete` of
   one that carries kausa's label, a `kausa.<fingerprint>` name and a `spec`) and, with incidents
   on, Incidents (`create`, and, for retention, a `delete` with UID and `resourceVersion`
   preconditions) and, with `--leader-lease`, a merge patch of the `spec` of its one Lease with a
   `resourceVersion` precondition. kausa never changes a workload and never acts on the model's
   answer. Amended by [0014](docs/adr/0014-explanation-history.md),
   [0019](docs/adr/0019-high-availability.md) and, `proposed`,
   [0028](docs/adr/0028-incident-resource-and-remediation-report.md) (point 8).
3. **Nothing reaches the model without passing `context.rs` and `redact.rs`**: only the fields of
   [Data sent to the provider](docs/reference/data-sent-to-the-provider.md), every name replaced
   and every free text redacted ([0008](docs/adr/0008-data-leaving-the-cluster.md)).
4. **Redaction only ever gets stricter.** A change that lets through a value a rule caught before
   needs an ADR. This covers the model's answer: running fewer rules on it needs an ADR.
5. **Cluster text is data, never instructions**: the model gets no tools, and its answer must
   match the schema, is cleaned and passes the pattern redaction rules `R003`–`R014` before it is
   logged, stored or written ([0022](docs/adr/0022-redact-the-answer.md)), and any summary, cause
   or step in which the shapes `W001`–`W006` match, as written to a sink after the names are
   mapped back, is replaced by kausa's fixed notice, with `low` confidence, in every sink, before it is logged,
   stored or written; checking less needs an ADR
   ([0025](docs/adr/0025-withhold-destructive-commands.md)). Code that parses untrusted text
   (cluster text, the model's answer, an Explanation read back, the endpoint's error body) never
   panics on any input; the fuzz targets check it. The endpoint's error body and its
   `Retry-After` header are untrusted: kausa reads the body only by fixed keys, compares what it
   reads with fixed lists, and derives only a cause it defines and a clamped delay; the start of
   the body stays in kausa's log only ([0020](docs/adr/0020-check-the-endpoint.md)). Nothing read
   back from an Explanation, an Incident or the Lease is logged, sent or reused (the Lease's holder
   is only compared with kausa's own identity, and its version echoed as the next precondition; of
   an Incident the sweep lists, the namespace and name only build its delete's path, the UID and
   `resourceVersion` are echoed only as that delete's preconditions, and its `creationTimestamp`
   only ranks it; the list's `continue` token is only sent back to ask for the next page). The
   `metadata.uid` of the answer to kausa's own create of an Incident is the one exception: it is
   parsed as a UID and kept, as 16 bytes, only in the table of created Incidents, never logged, sent
   or reused otherwise; the rest of that answer is read under a cap of 64 KiB and dropped
   ([0028](docs/adr/0028-incident-resource-and-remediation-report.md), point 8, `proposed`).
6. **One call per incident per process, within the budget, and one explanation at a time;
   nothing the cluster, the endpoint or the webhook does may stop the watches without
   `--leader-lease`, a notification never delays an explanation or an Event, and a fact never
   waits for the model, the budget, the breaker or an explanation.** A namespace
   that refuses kausa (`403`) is logged and retried; it never stops the other namespaces, and
   kausa never widens its scope. With `--leader-lease`, only the holder of the Lease acts; losing
   it stops the watches in that replica and hands them to the other, and never ends the process.
   Three things stop the watches in every replica with no handover, failing closed as scaling to
   zero does: whoever can write kausa's Lease in the release namespace holding it under another
   identity (or changing its version faster than `LEASE_DURATION`, so no standby takes over from
   a dead leader); a deleted Lease or `<fullname>-leader` RoleBinding; and Lease calls failing for
   `RENEW_DEADLINE`, as in an API server outage, which also drops the records and settle progress
   a single replica keeps. One call per incident becomes one per incident per process: a
   failover, forced or not, may explain a still-failing incident once more
   ([0019](docs/adr/0019-high-availability.md)). While the endpoint fails (0020's breaker is not
   closed), kausa makes no call and takes no budget except probes, queued incidents one at a time
   after each hold, at least every 30 minutes while one waits; `rejected`, `too_large` and
   `no_content`, the causes kausa classifies as the request's, never move it, and
   `invalid_answer` counts as an answer; an incident is still called at most twice per process.
   `kausa check-endpoint` is not the controller: it sends one fixed synthetic request, reads
   nothing from the cluster, and no budget counts it ([0020](docs/adr/0020-check-the-endpoint.md)).
7. **Rules are pure**: `detect.rs`, `incident.rs`, `fingerprint.rs`, `fact.rs`, `redact.rs`,
   `incident_resource.rs`, `incident_resource/table.rs`, `remediation.rs`, `history/pure.rs`,
   `model/pure.rs` and `breaker.rs` do no I/O and read no clock of their own (the history's
   `now` comes in as a string, the breaker's and the table's as an `Instant`), so the same state
   gives the same incidents, fingerprints, redactions, merges, causes, breaker decisions,
   Incident specs and gate answers.
8. **No cache of whole objects**: watch events are reduced to records at once, and objects are
   never logged.
9. **kausa's log goes through `log.rs`**, so a value from the cluster cannot break a line.
10. **No `unsafe`** (`unsafe_code = "forbid"`), no new dependency without a reason that passes
    `cargo deny`, and everything pinned: crates by `Cargo.lock`, images by digest, Actions by
    SHA, downloads by SHA-256.
11. **Nothing reaches the webhook but the fields of [Data sent in
    notifications](docs/reference/data-sent-in-notifications.md)**: no field holds a log line, an
    event message or any part of the context sent to the model, and no real name but the
    namespace's and the object's. The webhook URL and token are secrets read from files, never
    logged ([0015](docs/adr/0015-notifications.md)).
12. **A fact statement holds only what kausa read or fixed in its binary**: a detection's name and
    id, a container name that is a DNS label (checked by kausa), a reason that is in the compiled
    list, integers (exit code, two counts, a revision), and fixed words; its bytes are letters,
    digits, spaces and `.,:()-`, and it is under 512 bytes. Never the sample's message, an event's
    note, a log line, the image, a label, a node, a name but the container's, or any text a model
    wrote. Changing what it may hold needs an ADR
    ([0027](docs/adr/0027-fact-statement-for-the-detections.md)). An Incident holds only what a
    fact statement may hold and the typed values of 0028's schema; changing what it may hold needs
    an ADR ([0028](docs/adr/0028-incident-resource-and-remediation-report.md), point 8,
    `proposed`).

## How each kind of change goes

Every change is a pull request with its tests ([Contributing to kausa](CONTRIBUTING.md)); a
feature that is not a rule starts with an issue and an ADR.

- **A detection or redaction rule** → [Add a detection or redaction
  rule](docs/how-to/add-a-rule.md).
- **A new flag** → `main.rs` and its help; [Command line and log
  format](docs/reference/command-line.md); a case in `tests/cli.rs`; the chart's values and
  [Helm chart](docs/reference/helm-chart.md) if it belongs in a Pod.
- **A new counter, gauge, histogram or log line** → `metrics.rs` (`COUNTERS`, `GAUGES` or
  `HISTOGRAMS`) or the call site, `tests/golden/metrics.prom`, [Command line and log
  format](docs/reference/command-line.md); an alert that reads it →
  `deploy/prometheus/kausa-alerts.yaml`, its unit test in `tests/prometheus/` and
  [Alerts](docs/reference/alerts.md).
- **Anything sent to the model** → an ADR first; then `context.rs`, [Data sent to the
  provider](docs/reference/data-sent-to-the-provider.md) and [SECURITY.md](SECURITY.md) in the
  same change.
- **Anything sent in a notification** → an ADR first; then `src/notification.rs` and its golden
  tests, [Data sent in notifications](docs/reference/data-sent-in-notifications.md) (additive only
  within `v2`, with a line in the [Changelog](CHANGELOG.md); anything else is a new schema
  version) and [SECURITY.md](SECURITY.md) in the same change.
- **A new permission** → an ADR first; then the `kausa.rules` helper in
  `charts/kausa/templates/_helpers.tpl`, which `templates/rbac.yaml` renders in the ClusterRole and
  in every Role (never a rule in one scope only), [Permissions](docs/reference/permissions.md),
  the `kubectl auth can-i` lists in `tests/e2e/run.sh` and [SECURITY.md](SECURITY.md). The one
  exception is a permission on kausa's own object in its release namespace (the Lease,
  [0019](docs/adr/0019-high-availability.md)), which lives in
  `templates/leader-election.yaml`, the same in both scopes.
- **The chart** → [Helm chart](docs/reference/helm-chart.md), and `tests/e2e/run.sh` if what it
  installs changes.
- **The Explanation's schema** (`charts/kausa/files/explanation-schema.json`) → additive only
  within `v1alpha1`, the constants in `src/history/pure.rs` and `src/prompt.rs` that its test
  ties it to, [Explanation resource](docs/reference/explanation-resource.md), and a line in the
  [Changelog](CHANGELOG.md), always ([0014](docs/adr/0014-explanation-history.md)).
- **A dependency** → its reason in the pull request, and `cargo deny --locked check` green.
- **A pinned tool, image or action** → [Maintain the repository](docs/how-to/maintain.md).

Then the checks of [Verification (same as CI)](AGENTS.md#verification-same-as-ci), a line in the
[Changelog](CHANGELOG.md) if users will notice, and the docs that describe what changed.
