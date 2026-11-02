# 0021. Refill the hourly call budget at a steady rate from a start burst of three, and let incidents wait for it in the queue, so that a restart does not refill it

- Status: accepted
- Date: 2026-11-02
- Builds on: [0001. Build a small, read-only explainer that writes one redacted explanation per
  incident](0001-scope-and-positioning.md) (the budget as the cost control), [0013. Offer a
  namespaced install mode: one watch pair and one Role per listed namespace, and nothing
  cluster-wide](0013-namespaced-install-mode.md) (one budget and one queue per release) and
  [0019. Offer an optional standby replica that takes over through a chart-created Lease, elected
  by kausa's own code](0019-high-availability.md) (`Budget` per process, kept across its terms;
  persistence rejected as editable input)
- Amends: 0019, "What each replica does" (`Budget` kept across terms: the windows are, and the
  bucket below is cut to the burst at each term start) and Consequences, "Bounds" ("restarts reset
  them, as today"); the `Budget` row of the failover table in
  [Architecture](../explanation/architecture.md). 0019 stays `accepted` and gets an `Update`
  paragraph when this ADR is accepted. No invariant of [ARCHITECTURE.md](../../ARCHITECTURE.md)
  changes in wording; "within the budget" in invariant 6 now means the bucket, the 60-minute cap
  and the daily cap below.
- Related: [0008](0008-data-leaving-the-cluster.md) and [0009](0009-least-privilege-rbac.md)
  (unchanged: nothing new is sent, no permission is added), [0015](0015-notifications.md)
  (notifications follow model calls, so they are paced too) and `0020-check-the-endpoint.md`
  (its breaker also holds incidents in the queue; see Consequences)

## Context

Status as of 2026-11-02 (v0.4.1):

- **Every process starts with a full budget.** `Budget` keeps the start times of the last day's
  calls (`src/budget.rs:19-24`), empty at construction (`:27-33`); `take` refuses at `per_hour`
  calls in the last 60 minutes or `per_day` in the last 24 hours (`:36-57`); `refund` pops the
  last one (`:60-62`). `run()` builds one per process (`src/run.rs:380`) and `step_down` keeps it
  (`:715-718`). A new process may start 30 calls at once (defaults 30 and 200,
  `src/main.rs:588-596`; range 0 to 1,000,000, `:199`), and its empty `Dedup` makes every
  still-failing incident new. SECURITY.md: "a restart starts them over" (line 165), "a kausa that
  restarts often can spend more than its daily cap" (lines 331-332).
- **Restarts happen:** upgrades (`strategy: Recreate`), drains, failovers, and `OOMKilled` at
  every start at the old 128 MiB limit with 10,000 Pods ([Richer context,
  measured](../research/2026-10-29-richer-context-measured.md), line 24; spend not recorded).
- **Over the budget, an incident is skipped at once,** after its log was read
  (`src/explain.rs:179-194`): the take refuses (`:225`), `ExplanationSkipped` with `budget`.
- **The queue:** one per term (`src/run.rs:767`), 16 jobs (`src/explain.rs:32`). A new incident
  drops the oldest, logged and counted with no Event (`src/run.rs:902-914`); delayed retries rejoin
  unchecked (`:895-898`), past 16. The loop passes at least every 5 s (`TICK`, `:68`). The only
  gauge is `kausa_leader` (`src/metrics.rs:96-100`).

The v0.5 plan approved on 2026-11-02 (theme 2) asks for a refill bucket with a small start burst,
the daily cap unchanged, waiting in the existing queue, two gauges, and no stored state; it
rejects seeding the budget from kausa's own Events or Explanations.

## Decision

### The bucket (`src/budget.rs`)

| Quantity | Value | Defaults (30/h, 200/day) |
|---|---|---|
| Interval `I` | `3600 s / --max-calls-per-hour`, whole nanoseconds | 120 s |
| Burst `B` (`START_BURST`, a constant) | `min(3, --max-calls-per-hour)` | 3 |
| Capacity | `--max-calls-per-hour` calls | 30, reached 54 min after a start with no call |
| Credit at process start | `B` calls | 3 |
| At each term start | Credit cut to at most `B` (a takeover after a standby) | 3 |

- **A call may start** only when the bucket holds a whole call, fewer than `--max-calls-per-hour`
  calls started in the last 60 minutes, and fewer than `--max-calls-per-day` in the last 24
  hours. The two windows are today's code and stay; `take` spends one call of credit.
- **Credit** accrues one call per `I` on the monotonic clock, whatever the phase, up to the
  capacity, held as one `Instant` (`full_at`, when the bucket is full): no float, no new state
  that grows, checked arithmetic that never panics.
- **`refund`** gives back both the start time and the credit, as if the take had not happened.
- **`check(now) -> Allowance`**, which changes nothing: `Now`; `Wait { until }` when only the
  bucket or the 60-minute cap stops the call; `Never(reason)` when the daily cap is spent or a cap
  is `0`, with today's messages. `take` keeps its signature and stays the authority.
- **No new flag and no chart value;** names, defaults and ranges stay. The help reads `Model calls
  allowed in any hour (default 30); each start allows 3 at once, then one every 3600/N seconds`,
  and `--max-calls-per-day`'s gains `per process`.

### The stated bound

Let `c(s)` be the credit, in calls, of the process that leads at `s` (at most
`--max-calls-per-hour`; at most `B` when `s` is a term start), and `K` the term starts in
`(s, s + T]`: process starts without `--leader-lease`, takeovers with it. A release starts at most
`c(s) + B × K + T / I` calls in `[s, s + T]`, and each process at most `--max-calls-per-hour` in
any 60 minutes and `--max-calls-per-day` in any 24 hours of its life. It rests on terms not
overlapping: 0019's two timing hypotheses with the Lease (no fencing), and `strategy: Recreate`
without it.

- **The daily cap holds per process only.** Across restarts a day holds at most
  `24 × callsPerHour + 3 × K + c(s)`. The documentation says: for the daily cap to hold whatever
  the restarts, up to the bursts, set `budget.callsPerHour <= budget.callsPerDay / 24`.
- **Example at the defaults** (hypothesis: kubelet's crash back-off caps at one restart per 5 min,
  and each process lives past the 60 s settle window): at most 66 calls an hour (1,584 a day),
  against 360 (8,640) in v0.4.1.

### Waiting in the queue (`src/run.rs`, `src/explain.rs`)

With an endpoint, before a free explanation slot takes a job (`src/run.rs:801-805`), `lead()`
asks `check(now)`. A due history sweep still comes first; `next_in_slot` does not change.

| Allowance | What takes the slot | Outcome |
|---|---|---|
| `Now` or `Never` | The oldest job, as today | Explained, cached or skipped as today |
| `Wait` | The first queued job with a fresh `Dedup` entry (`Dedup::is_fresh`, new, removes nothing), out of order | Served from the cache, no call |
| `Wait`, none fresh, 16 queued | The oldest job | `ExplanationSkipped`, reason `budget`, `the hourly call budget is spent`, written by the slot |
| `Wait`, otherwise | Nothing | The jobs wait; the next pass checks again |

- **`explain::run` checks before it reads the log.** After the cache, a job the budget would
  refuse is skipped with reason `budget` and no log read; `take` stays at `src/explain.rs:225`.
- **No timer.** A job starts at most one tick (5 s) after its call is due.
- **A spent daily cap and a cap of `0` never wait:** a rolling day frees one call at a time, so
  the queue would hold stale work for hours. They skip at once, as today.
- **Drops at the tick stay silent:** a new incident that finds 16 queued (several settled in one
  tick, or the slot busy) drops the oldest, logged and counted as today. `explanation dropped`
  gains `waiting_for_budget`.
- **Retries obey the limit:** a delayed retry rejoins through the same drop-the-oldest rule, so the
  queue never holds more than 16.
- **Log:** `explanations waiting` (info; `reason`, `queued`, `next_call_in_seconds`), once when
  a job first waits after the queue was last empty. Nothing per pass, nothing per job.

### Standby, failover and deduplication

- **Standby:** credit accrues but is cut to `B` when the term starts, so a takeover, forced or
  not, brings at most 3 calls at once. The windows are kept across terms, as 0019 decided; the
  queue is still lost at a step-down.
- **Deduplication** is unchanged (1024 entries, `--dedup-hours`, per process). A cached repeat
  never waits behind the budget, and queued jobs of one fingerprint cost one call.

### Metrics (`src/metrics.rs`)

Two gauges after `kausa_leader`, with no namespace, Pod, owner or Lease label, through a new
`Metrics::set_gauge`. A gauge never set is not rendered.

| Gauge | Labels | Value | Rendered |
|---|---|---|---|
| `kausa_budget_calls_remaining` | `window` (`hour`, `day`) | `hour`: whole calls in the bucket within the 60-minute cap (on a standby, at most `B`: what a takeover would bring); `day`: `--max-calls-per-day` minus the calls of the last 24 hours | With `--endpoint`; refreshed when a metrics connection is accepted, so a standby's value is current |
| `kausa_explanation_queue_length` | | Queued jobs, 0 to 16: not the one in the slot, nor delayed retries | With `--endpoint` or `--dry-run`; 0 on a standby and after a step-down |

Help texts: `Model calls this process may start now, by window (hour: the refill bucket within the
60-minute cap; day: the 24-hour cap).` and `Incidents waiting for an explanation (at most 16).`

**Unchanged:** no persistence (no Lease field, ConfigMap or read-back of Events or Explanations),
no permission, request field, destination, skip reason, counter or label.

## Alternatives

- **Status quo plus the provider's spending cap:** keeps a restart-multiplied spend that whoever
  can crash kausa drives. The provider's cap stays advised either way.
- **Persist the budget in the Lease, a ConfigMap or the history:** editable input (0019), and a
  new write. **Seed it from kausa's own Events or Explanations:** reads its output back, depends on
  an unverified field selector, and forged objects steer it (the plan rejects it).
- **No start burst (empty at each term start):** the bound loses `B × K`, but every install,
  upgrade and failover waits `I` (an hour at `callsPerHour: 1`) for its first call. The plan asks
  for a burst; the maintainer confirms 3 or 0 at acceptance, a one-constant change.
- **A capacity of 3 forever:** the bound loses `c(s)`, but after hours of quiet a node failure
  with 20 incidents gets 3 calls, then one per `I`, and the queue drops the rest, which v0.4
  explained within its 30.
- **The bucket without the 60-minute cap:** up to twice the cap in one hour after a quiet hour.
- **A rate from the daily cap** (`min(N/3600, D/86400)`): one call per 432 s at the defaults for
  everyone; the `callsPerHour <= callsPerDay / 24` advice gives it to those who want it.
- **A cold-start allowance only** (`B` in the first hour): a restart every 59 minutes gets the
  full window again. **Skip while the bucket is empty:** upgrades turn explanations into skips.
- **An Event for every dropped job:** a write in the tick arm can hold the loop for `WRITE_BOUND`
  (10 s) per drop while the watches wait; at 16 the slot writes it instead.
- **A timer at the next token, a burst flag, a per-namespace bucket, a larger queue:** a
  `select!` arm to save 5 s; a setting nothing measured asks for; a fairness that 0013 and "The
  budget is shared" do not promise; more memory.

## Consequences

- **Slower right after a start.** After an install, upgrade, restart or takeover: 3 calls at once,
  then one per `I`; ten still-failing incidents take about 14 minutes instead of about one, and
  the 16th queued waits about 26. A job waits at most until the 60-minute cap frees a place plus
  its position times `I`, and it is explained as it was when it settled, with the log read when
  its call starts. After 54 quiet minutes a storm behaves as in v0.4.1.
- **Fewer `ExplanationSkipped` Events with reason `budget`:** only the daily cap, caps of `0` and
  jobs pushed out of a full waiting queue give one. Such a skip reads no log, and may now come
  where `context_too_large` came before.
- **Security** ([SECURITY.md](../../SECURITY.md)): a tenant who can crash or restart kausa adds at
  most 3 calls a start; whoever flips the Lease gets at most 3 a takeover. The gauges show any
  peer of the metrics port when the budget is spent, with no names. A tenant who floods the queue
  pushes other namespaces' waiting incidents out ("The budget is shared"). A paused VM whose
  monotonic clock stops refills less, which fails safe. New state: one `Instant`.
- **`0020-check-the-endpoint.md`, if accepted:** the slot admits a job only if both the breaker
  and the budget allow it, a half-open probe takes a token, and a job pushed out of a full queue
  while a gate holds is skipped by the slot with that gate's reason. Both ADRs use one admission
  function and the same silent tick-time drop.
- **Tests:** unit tests of the bucket with injected instants, and a seeded schedule test of the
  bound (no new crate); `src/run.rs` and `src/explain.rs` tests of each admission row and of no
  log read on a budget skip; an e2e case in kind (`callsPerHour=360`, `settleSeconds=5`, 20
  crash-looping workloads, kausa's Pod deleted every 60 s for 10 minutes) whose request count
  stays within the bound, with `K` from the Pod starts. Existing e2e runs set `callsPerHour=3600`.
- **Documentation, in the implementation pull request:** CHANGELOG, Architecture, SECURITY.md,
  Command line and log format, Helm chart, `values.yaml`, Install kausa with Helm (two example
  alerts), ARCHITECTURE.md's codemap, README, and 0019's `Update`.
- **Hypotheses:** kubelet's back-off (10 s doubling to 5 minutes; only the example uses it); that
  terms do not overlap (above); that a 5 s start delay and a burst of 3 suit operators, to be
  sampled from both gauges in the soak planned against the v0.5 release candidate.

Related work, without an ADR: an explicit, smaller `page_size` for the three watches (today
`watcher::Config::default()`, `src/run.rs:461-470`: 500 in kube-runtime 4.2.0), chosen in a dated
research note at 10,000 Pods and with a forged page of zero-replica Deployments, against 192Mi.
