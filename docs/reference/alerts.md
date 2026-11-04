# Alerts

The Prometheus alerting rules in
[`deploy/prometheus/kausa-alerts.yaml`](../../deploy/prometheus/kausa-alerts.yaml): what each
alert means, its expression and what to do when it fires.

Status as of 2026-11-04 (released in v0.6.0): eleven alerts in five groups, all on kausa's own
metrics ([Command line and log format](command-line.md#metrics)). The `ci` target of the
`Dockerfile` checks the file with `promtool check rules --lint=all --lint-fatal` and runs its unit
tests, [`tests/prometheus/kausa-alerts.test.yaml`](../../tests/prometheus/kausa-alerts.test.yaml),
with `promtool test rules`, both with promtool 3.15.1. The tests evaluate each threshold, window
and `for` one minute either side of its boundary, at one sample and one evaluation a minute; on
2026-11-03, each of 61 single changes to the rules (every window and `for` moved by a minute, every
count threshold by one step, and the like) made at least one test fail. The thresholds come from
kausa's own limits (the queue of 16, the breaker's longest hold, the watch's backoff); the one of
`KausaWatchErrors` also rests on a single measured cluster, so it is a hypothesis elsewhere.

## Use the rules

- **With Prometheus**, copy the file next to your configuration and list it under `rule_files`:

  ```yaml
  rule_files:
    - kausa-alerts.yaml
  ```

- **With the Prometheus Operator**, put the file's `groups` under the `spec` of a
  `PrometheusRule`, with the labels your Prometheus selects rules by:

  ```yaml
  apiVersion: monitoring.coreos.com/v1
  kind: PrometheusRule
  metadata:
    name: kausa
    namespace: kausa
  spec:
    groups: []  # the groups of deploy/prometheus/kausa-alerts.yaml
  ```

  The chart installs no `PrometheusRule` and no `ServiceMonitor` or `PodMonitor`: each needs the
  Operator's CustomResourceDefinitions.
- **Scrape each Pod.** With two replicas, each Pod has its own metrics, and the alerts read both:
  scrape the Pods, as [Install kausa with Helm](../how-to/install-with-helm.md#run-a-standby-replica)
  shows, not the Service, which answers from one Pod at a time.
- **Labels.** Every alert carries one label, `severity: warning`: kausa explains failures, it keeps
  no workload up. Route on `alertname` or add your own labels in your copy.
- **Target labels.** Nine alerts read one process's series, and the three of them that combine two
  metrics match them with `ignoring(...)` on kausa's own labels, so they work whatever labels your
  scrape configuration adds. `KausaNoLeader` and `KausaLeaderFlapping` add up a release's Pods
  `by (namespace, job)`; that your scrape configuration sets both labels, and that they tell two
  releases apart, is a hypothesis (not checked against a `PodMonitor`). If yours differ, replace
  `namespace, job` in both expressions with the labels that name one release.
- **What the rules rely on**, all set by `kausa run` at start: `kausa_endpoint_state{state="closed"}`
  at `1` in every mode, `kausa_explanation_queue_length` at `0` whenever kausa explains, and the
  counters with a fixed set of label values at `0` for each value, in the modes that can count
  them ([Command line and log format](command-line.md#metrics)). A counter series that appears
  for the first time at `1` reads as no increase, so without those `0` series the first skip,
  failed call or failed notification after a start would fire nothing. Load the rules with the
  kausa release they come with: v0.5.1 and earlier have no `kausa_watch_restarts_total`, so
  `KausaWatchRefused` stays silent there.
- **Runbook links.** Each `runbook_url` points to this page on `main`, so a cluster that runs an
  older release may read newer text.

| Alert | Group | Severity | `for` | Fires when |
|---|---|---|---|---|
| [`KausaModelEndpointFailing`](#kausamodelendpointfailing) | `kausa-model` | `warning` | none | The breaker has not been closed for 20 minutes and a call failed in the last 45 |
| [`KausaNoSuccessfulModelCall`](#kausanosuccessfulmodelcall) | `kausa-model` | `warning` | `10m` | Calls failed in the last hour and none was answered with a chat completion in that hour |
| [`KausaDailyCallBudgetSpent`](#kausadailycallbudgetspent) | `kausa-model` | `warning` | `10m` | The acting replica has spent its daily call budget |
| [`KausaExplanationQueueNearlyFull`](#kausaexplanationqueuenearlyfull) | `kausa-explanations` | `warning` | `15m` | 12 or more of the 16 queue places are taken |
| [`KausaExplanationsBehind`](#kausaexplanationsbehind) | `kausa-explanations` | `warning` | none | The queue has not emptied for 45 minutes |
| [`KausaExplanationsSkipped`](#kausaexplanationsskipped) | `kausa-explanations` | `warning` | none | An incident was skipped in the last hour, one alert per reason |
| [`KausaWatchErrors`](#kausawatcherrors) | `kausa-watches` | `warning` | `15m` | A watch had more than 20 errors in 15 minutes |
| [`KausaWatchRefused`](#kausawatchrefused) | `kausa-watches` | `warning` | `20m` | The API server keeps refusing a watch (`403`) |
| [`KausaNoLeader`](#kausanoleader) | `kausa-replicas` | `warning` | `2m` | A release has no acting replica, or two |
| [`KausaLeaderFlapping`](#kausaleaderflapping) | `kausa-replicas` | `warning` | none | The Lease changed hands 7 times or more in an hour |
| [`KausaNotificationsFailing`](#kausanotificationsfailing) | `kausa-notifications` | `warning` | none | A notification failed in the last hour |

## kausa-model

### KausaModelEndpointFailing

```promql
max_over_time(kausa_endpoint_state{state="closed"}[20m]) == 0
  and ignoring(state, outcome)
increase(kausa_model_calls_total{outcome="error"}[45m]) > 0
```

`for`: none; the 20-minute window is the delay.

**What it means:** kausa stopped calling its model endpoint because calls kept failing: the
breaker ([Command line and log format](command-line.md#the-breaker)) has been open or half open
for 20 minutes, and a call failed in the last 45. Incidents wait in the queue, and the oldest of a
full queue is skipped (`endpoint_unavailable`).

**Why these windows:** the longest hold is 30 minutes, and a probe takes at most
`--model-timeout` (600 seconds at most), so a breaker that keeps failing counts a failed call in
every 45 minutes. With one sample a minute, the tests pin it to the minute: it fires 20 minutes
after the last sample that saw the breaker closed, as long as a call failed in the last 45
minutes; one failed call is enough. The breaker opens between that sample and the next, so the
delay after it opens is up to one scrape interval shorter.

**When it does not fire:** on a standby, whose breaker always reads `closed`; on a quiet cluster
that sits half open after an outage, 45 minutes after the last failed call; when a call closes the
breaker again. A passing `helm test` does not resolve it: the check is another process and does not
close the controller's breaker.

**What to do:**

1. Read kausa's `endpoint unavailable` log line: its `cause`, `class` and `hint` name the failure
   and the flag or chart value to fix ([Command line and log format](command-line.md#causes)).
2. Run the endpoint check, which sends one synthetic request and says which step failed:
   `helm test <release> -n <namespace> --logs`.
3. Fix what the hint names (`model.endpoint`, `model.name`, the Secret in
   `model.apiKey.existingSecret`, the provider's credit, `networkPolicy.endpoint`), or, for a
   `transient` cause, wait for the provider: the next probe closes the breaker on success.

**Tests:** `endpoint failing`, `endpoint half open and quiet`.

### KausaNoSuccessfulModelCall

```promql
increase(kausa_model_calls_total{outcome="error"}[1h]) > 0
  unless ignoring(outcome)
(
    kausa_model_last_success_timestamp_seconds > 0
  and
    kausa_model_last_success_timestamp_seconds > time() - 3600
)
```

`for`: `10m`.

**What it means:** calls to the model failed in the last hour, and none was answered with a chat
completion in that hour, or ever since kausa started.

**Why:** it catches what the breaker never sees. The causes kausa classifies as the request's
(`rejected`, `too_large`, `no_content`) never move the breaker, so an endpoint that rejects every
request keeps it closed and `KausaModelEndpointFailing` quiet. It also fires during an outage,
after that alert: both name the endpoint. `kausa_model_last_success_timestamp_seconds` is `0`
before the first chat completion, and `> 0` keeps "never" from reading as recent.

**When it does not fire:** while no call fails; when a chat completion, valid or not, came in the
last hour; in a dry run and without `--endpoint`, which make no call.

**What to do:**

1. Read kausa's `endpoint error`, `explanation skipped` and `endpoint unavailable` log lines. The
   `cause` and `hint` of the first and the last name the flag or chart value to fix
   ([Command line and log format](command-line.md#causes)).
2. For `rejected` with structured output on, the endpoint may not support it: set
   `model.jsonSchema=false` (`--no-json-schema`). For `too_large`, lower `budget.maxInputTokens`.
   For `no_content`, try another model.
3. Run `helm test <release> -n <namespace> --logs` to check the endpoint, the key and the model
   with one request.

**Tests:** `no successful call`, `calls stop failing`, `a recent success`.

### KausaDailyCallBudgetSpent

```promql
kausa_budget_calls_remaining{window="day"} == 0
  and ignoring(window)
kausa_leader == 1
```

`for`: `10m`.

**What it means:** the replica that acts has spent `--max-calls-per-day` (`budget.callsPerDay`):
it makes no model call until the oldest of the last 24 hours' calls is 24 hours old. New
incidents wait in the queue, and those that reach the call are skipped (`budget`).

**Why:** a former leader that spent its day still reads `0`, hence `kausa_leader == 1`.
`ignoring(window)` matches the two series of one process whatever target labels the scrape adds.
`window="hour"` at `0` is normal under load and has no alert.

**When it does not fire:** on a standby, and without `--endpoint` or with `--dry-run`, where the
budget gauge is not rendered.

**What to do:**

1. Find what fails so often: kausa's `incident` log lines and `kausa_incidents_total` by
   `detection`.
2. Fix those workloads, or raise `budget.callsPerDay`. For the daily cap to hold across restarts,
   keep `budget.callsPerHour` at most `budget.callsPerDay / 24`
   ([Helm chart](helm-chart.md)).

**Tests:** `daily budget spent`.

## kausa-explanations

### KausaExplanationQueueNearlyFull

```promql
kausa_explanation_queue_length >= 12
```

`for`: `15m`.

**What it means:** 12 or more of the 16 places of the explanation queue have been taken for 15
minutes: incidents come in about as fast as the budget or the breaker lets them out. When the
queue is full, the oldest incident is dropped (`kausa_explanations_dropped_total`), or skipped as
`endpoint_unavailable` while the breaker is open.

**Why 12 and 15 minutes:** a start with a full queue drains below 12 within a few minutes (a burst
of 3 calls, then one every 120 seconds at the default `budget.callsPerHour` of 30).

**When it does not fire:** with 11 or fewer waiting, however long; on a standby and after a
step-down, which read `0`; without `--endpoint` and `--dry-run`, where the gauge is not rendered.

**What to do:**

1. Read kausa's `explanations waiting` log line: its `reason` says whether the hourly budget or the
   breaker holds the queue.
2. For the budget, raise `budget.callsPerHour` (keeping it at most `budget.callsPerDay / 24`).
3. For the breaker, see [`KausaModelEndpointFailing`](#kausamodelendpointfailing).
4. If many workloads fail at once, kausa's `incident` lines name them.

**Tests:** `queue nearly full`.

### KausaExplanationsBehind

```promql
min_over_time(kausa_explanation_queue_length[45m]) > 0
```

`for`: none; the 45-minute window is the delay.

**What it means:** incidents have waited for 45 minutes without the queue ever emptying.

**Why 45 minutes:** room for a full queue to drain after an upgrade at the defaults (13 calls of
120 seconds after the start burst, plus the settle window). It relies on kausa setting the queue
gauge to `0` at start: a series that started at `1` would fire at once, since `min_over_time` sees
only the samples it has.

**When it does not fire:** when the queue empties at least once in 45 minutes; on a standby and
after a step-down.

**What to do:**

1. Read kausa's `explanations waiting` log line for the reason: the hourly budget or the breaker.
2. Then as for [`KausaExplanationQueueNearlyFull`](#kausaexplanationqueuenearlyfull).

**Tests:** `explanations behind`.

### KausaExplanationsSkipped

```promql
increase(kausa_explanations_skipped_total[1h]) > 0
```

`for`: none.

**What it means:** kausa skipped an incident in the last hour, one alert per `reason`. Each
skipped incident has an `ExplanationSkipped` Event (unless `--no-events`) and an `explanation
skipped` log line with the reason in words.

**Why:** a skip is an incident nobody got an explanation for. The counter's series start at `0`
whenever kausa explains, so the first skip after a start fires too.

**When it does not fire:** when nothing was skipped in the last hour; an alert resolves about an
hour after its last skip.

**What to do,** by `reason`:

| `reason` | Why kausa skipped | What to do |
|---|---|---|
| `budget` | The daily cap is spent, a cap is `0`, or the hourly budget ran out between the check and the call | See [`KausaDailyCallBudgetSpent`](#kausadailycallbudgetspent); `budget.callsPerDay`, `budget.callsPerHour` |
| `context_too_large` | The context does not fit `budget.maxInputTokens` (`--max-input-tokens`), or the endpoint refused the request as too large (`413`, `context_length_exceeded`) | Raise `budget.maxInputTokens` if the model's context allows it; after a refusal by the endpoint, lower it or use a model with a larger context |
| `endpoint` | A configuration error, a request the endpoint rejected or answered with no content, or a transient error on the second call | The `cause` and `hint` of the `endpoint error` line ([Command line and log format](command-line.md#causes)); `helm test <release> -n <namespace> --logs` |
| `invalid_answer` | The model answered, but not with kausa's JSON | A larger model, or `model.jsonSchema=false` if the server ignores `response_format` |
| `endpoint_unavailable` | The oldest of 16 queued incidents, while the breaker was open | See [`KausaModelEndpointFailing`](#kausamodelendpointfailing) |

**Tests:** `explanations skipped`.

## kausa-watches

### KausaWatchErrors

```promql
increase(kausa_watch_errors_total[15m]) > 20
```

`for`: `15m`.

**What it means:** one of kausa's watches (`pods`, `events` or `deployments`; with `--namespace`,
every listed namespace's watch of that kind together) keeps failing, so kausa may not see the
cluster.

**Why 20:** a watch that fails without a break retries every 30 seconds once its backoff has grown:
about 30 errors in 15 minutes. The only benign rate measured, 17 restarts of the Event watch in a
22-minute run on k3s ([Memory and CPU on a Raspberry Pi
4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)), is about 12 in 15 minutes. 20 sits
between them; on other clusters it is a hypothesis. `increase` extrapolates to the edges of its
window, so a steady 1.35 errors a minute reads 20.25 and fires once held for 15 minutes, while 1.3
a minute (19.5) never does.

**When it does not fire:** for errors a working watch recovers from, under 20 in 15 minutes.

**What to do:**

1. Read kausa's `pod watch error`, `event watch error` and `deployment watch error` log lines:
   their `namespace` and `error` say which watch fails and why.
2. A `forbidden` error is a refusal: see [`KausaWatchRefused`](#kausawatchrefused).
3. Otherwise, check that kausa reaches the API server (with `networkPolicy.enabled`,
   `networkPolicy.apiServer`) and the API server's health.

**Tests:** `watch errors`.

### KausaWatchRefused

```promql
increase(kausa_watch_restarts_total[10m]) > 0
```

`for`: `20m`.

**What it means:** the API server keeps refusing one of kausa's watches with a `403`, and kausa
keeps starting a new watcher for it: kausa sees nothing that watch would bring, usually in one
namespace. A watch refused before its first list also keeps kausa not Ready.

**Why 20 minutes:** a RoleBinding that `helm upgrade` recreates gives a few restarts, then none;
one that keeps coming for 20 minutes is a refusal that lasts. A single restart never fires.

**When it does not fire:** after a short refusal; it resolves about 10 minutes after the last
restart.

**What to do:**

1. Read kausa's watch error lines whose `error` says `forbidden`: their `namespace` names the
   refused one.
2. Restore kausa's Role or RoleBinding there: `helm upgrade` with the same values recreates them
   ([Permissions](permissions.md)). If the namespace is gone on purpose, remove it from
   `rbac.namespaces`.

**Tests:** `watch refused`.

## kausa-replicas

### KausaNoLeader

```promql
sum by (namespace, job) (kausa_leader) != 1
```

`for`: `2m`.

**What it means:** in one release, no replica acts (`0`), or two do (`2`, never expected). With
`replicaCount: 2`, nobody acts when the Lease is held under another identity, has been deleted, or
cannot be renewed (an API server outage, a deleted `<fullname>-leader` RoleBinding): kausa then
explains nothing, on purpose, rather than act without the Lease.

**Why:** `kausa_leader` is `1` while a process acts (it holds the Lease, or runs without
`--leader-lease`) and `0` on a standby, so a release's sum is `1`. The sum is per `namespace` and
`job`, so that two releases scraped by one Prometheus do not add up (a hypothesis on your labels:
see [Use the rules](#use-the-rules)).

**When it does not fire:** with one replica, which always reads `1`; when no Pod of the release is
scraped at all, since the sum then has no series (Prometheus's `up` covers that).

**What to do:**

1. See who holds the Lease:
   `kubectl -n <namespace> get lease <fullname>-leader -o jsonpath='{.spec.holderIdentity}'`.
2. Read both Pods' logs: `lease error` lines carry a `hint` (`helm upgrade` for a missing Lease,
   the permission for a `403`); both replicas logging `standby` means nobody holds it.
3. A deleted Lease or RoleBinding comes back with `helm upgrade` and the same values. A Lease held
   under another identity was written by someone who can write Leases in that namespace
   ([Security policy](../../SECURITY.md)).

**Tests:** `no leader`.

### KausaLeaderFlapping

```promql
sum by (namespace, job) (increase(kausa_leadership_changes_total{to="leader"}[1h])) >= 7
```

`for`: none.

**What it means:** kausa's Lease changed hands more than 6 times in an hour in one release.
Failures, or someone writing the Lease, move it between the replicas, and a failover may explain a
still-failing incident once more.

**Why `>= 7`, not `> 6`:** `increase` extrapolates to the edges of its window, so six takeovers in
the hour of a release whose counter is already above `0` read a little more than 6 (about 6.1 to
6.3 at one sample a minute, by the arithmetic of the extrapolation; the tests showed that six
fire under `> 6`). `>= 7` keeps six silent while the extrapolation adds less than one sixth;
that a shorter scrape interval extrapolates less is a hypothesis.

**When it does not fire:** with six takeovers or fewer in the hour; with one replica.

**What to do:**

1. Read both Pods' `leadership lost` lines: `reason` `not renewed in time` points at the API server
   or the node (renewals that do not succeed within 20 seconds), `held by another` at someone
   writing the Lease.
2. Read the `lease error` lines and the Pods' restarts (`kubectl -n <namespace> get pods`).

**Tests:** `leader flapping`.

## kausa-notifications

### KausaNotificationsFailing

```promql
increase(kausa_notifications_total{outcome="failed"}[1h]) > 0
```

`for`: none.

**What it means:** kausa could not post a notification to its webhook in the last hour. A failed
notification is not tried again until its incident comes back, and it never shows in an Event.

**Why:** the counter's series start at `0` with `--notify-url-file`, so a lone first failure fires
too. `kausa_notifications_total{outcome="dropped"}` counts notifications pushed out of the queue
of 16 by a burst, and `kausa_notifications_retried_total` second attempts; neither has an alert.

**When it does not fire:** without notifications; it resolves about an hour after the last
failure.

**What to do:**

1. Read kausa's `notification failed` log lines: `reason` names the status the webhook answered, a
   redirect, or a request that failed.
2. `the webhook answered 404` points at a wrong URL or token in the Secret of
   `notifications.existingSecret`; a redirect, at a URL that is not the final one; a failed
   request, with `networkPolicy.enabled`, at `networkPolicy.notifications.to`
   ([Send notifications to Slack, Telegram or a
   webhook](../how-to/send-notifications.md)).

**Tests:** `notifications failing`.

## What has no alert

The relists (`kausa_watch_relists_total`, normal after an expired resource version), the call
duration histogram (no latency objective exists), the record and cache gauges (no threshold has
been measured) and `kausa_answer_redactions_total`, whose alert waits for a measured rate ([0022. Pass the model's
answer through the pattern redaction rules before it is written anywhere, and redact the query of
an HTTP request line](../adr/0022-redact-the-answer.md)) and `kausa_answers_withheld_total`, whose
alert waits for the measurement of [0025. Replace the model's text that holds one of six listed
destructive command shapes with a fixed notice, show the notice first, and give that answer `low`
confidence](../adr/0025-withhold-destructive-commands.md): it ships only if no benign answer is
withheld.
