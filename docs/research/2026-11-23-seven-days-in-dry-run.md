# Seven days of v0.5.1 in dry-run

The dry-run soak of [Run the soak](../how-to/run-the-soak.md) ended on 2026-11-23: over 168 hours,
two replicas of v0.5.1 that watched a whole three-node ARM cluster of about 100 Pods did not
restart, stayed under 10 MiB of working set and did not grow. The soak gate of that page's
decision rule passes in the terms a dry-run soak can check; the scale gate, run the next day,
failed ([The scale gate](2026-11-24-the-scale-gate.md)), so the chart's memory limit stays at
`192Mi`.

Measured from 2026-11-16T16:13:17Z to 2026-11-23T16:13:17Z, once, on one cluster: a data point,
not a benchmark.

## Result

`tests/soak/summarize.py --dry-run` gives **S1 VALID, S2 PASS, S3 PASS**. The leader is the
replica that held the Lease, in every sample of the window; the standby is the other one.

| | Leader | Standby | The rule |
|---|---|---|---|
| Coverage of the 30-second slots | 94.3% | 94.3% | S1: at least 90% |
| Restarts in the window | 0 | 0 | S1: 0 |
| `OOMKilled`, cAdvisor's OOM events | none, 0 | none, 0 | S2: none |
| Working set, highest | 9.0 MiB | 3.3 MiB | S2: at most 32 MiB |
| cAdvisor's highest usage | 16.1 MiB | 8.1 MiB | S2: at most 64 MiB |
| Slope of the hourly median working set, hours 24-168 | -0.102 MiB a day | -0.074 MiB a day | S3: at most 0.1 MiB a day |
| Highest hourly median of the last 24 hours, against hours 24-48 | 0.90 MiB lower | 0.64 MiB lower | S3: at most 1 MiB higher |
| Resident memory, highest | 4.1 MiB | 0.3 MiB | not gated |
| CPU, mean and p99 of the 30-second samples | 0.4m, 0.8m | 0.2m, 0.4m | flagged above 100m |

The working set by day of the window, median and highest, in MiB:

| Day | Leader, median | Leader, highest | Standby, median | Standby, highest |
|---|---|---|---|---|
| 1 | 6.5 | 7.3 | 2.4 | 3.3 |
| 2 | 5.8 | 7.3 | 1.8 | 2.6 |
| 3 | 5.8 | 7.8 | 1.1 | 2.5 |
| 4 | 7.5 | 9.0 | 1.0 | 1.9 |
| 5 | 5.7 | 7.7 | 1.2 | 1.9 |
| 6 | 5.7 | 6.7 | 1.2 | 1.9 |
| 7 | 5.7 | 6.4 | 1.2 | 1.9 |

- **What it decides.** Of the rule's outcomes, "S1 INVALID → repeat the soak" and "Any FAIL →
  keep `192Mi`" do not apply, so the scale gate (C1 and C2, in kind) is the next step. It was
  run on 2026-11-24 and C1 failed ([The scale gate](2026-11-24-the-scale-gate.md)), so the
  chart's limit stays at `192Mi`.
- **S1 is read as a dry-run soak reads it.** In dry-run kausa calls no model, so the breaker, the
  successful calls and the queue that S1 names were not checked ("A dry-run soak" in [Run the
  soak](../how-to/run-the-soak.md)): the queue's highest length was 0, the breaker was never
  open, and the budget gauges were not reported.
- **The chart's requests** (10m CPU, 32 MiB) stay well above both replicas.

## Setup

- The cluster: Kubernetes v1.36.5 (k3s), three Raspberry Pi 4 Model B (8 GB) nodes on Debian 13,
  wired Gigabit Ethernet, with 102 to 104 Pods and 0 to 3 Warning Events at the sampler's
  cluster counts, every 600 seconds. The cluster runs its owner's own workloads; no node
  restarted in the window.
- kausa v0.5.1, the published image pinned by digest, with the chart of the tag `v0.5.1`:
  `rbac.scope=cluster`, `replicaCount: 2` and `dryRun=true`, installed by the cluster's deploy
  tool, with no fake endpoint, no notifications and no seeded failures.
- `tests/soak/sample.py` every 30 seconds from a machine outside the cluster, started with
  `tests/soak/launch.py`: 38,732 lines in the window, 19,001 samples of each Pod out of 20,160
  slots. cAdvisor's highest usage and its OOM events were read every 600 seconds, 941 times for
  each Pod.
- The window is the third of this soak. The first two are invalid because the sampler stopped
  reading the cluster ([2026-11-18 — The soak sampler on
  macOS](../journal/2026-11-18-the-soak-sampler-on-macos.md)). The Pods were not restarted
  between windows: the leader's counters were already above zero at the window's first sample.

## Observations

- **One gap of 9.6 hours** holds nearly all the lost coverage: from 06:38 to 16:15 UTC on
  2026-11-21 the sampler's machine had no network, and its 730 reads of the Pods in those hours
  failed. Outside the gap, 6 slots were lost. The Pods and their restart counts were the same
  before and after it, and the leader's counters went on from where they were.
- **Failed reads of the metrics:** 242 samples have the Pod's memory and CPU but not kausa's
  metrics, 39 of the leader and 203 of the standby. The sampler's log names `MethodNotAllowed`
  and `i/o timeout`. The first is v0.5.1's own `405` to a request that arrives in pieces through
  the API server's proxy, fixed in v0.6.3 ([2026-11-18 — The soak sampler on
  macOS](../journal/2026-11-18-the-soak-sampler-on-macos.md), "The failed reads of the
  metrics"). The coverage counts such a sample.
- **The standby's `restartCount` was 1 at the window's first sample**, from a restart before the
  window whose termination reason the kubelet reports as `Unknown`. It did not rise in the
  window, so S1 does not count it.
- **Event watch errors:** `kausa_watch_errors_total{watch="events"}` rose by 7,644 on the leader,
  between 473 and 1,495 a day; the Pod and Deployment watches had none. Sixty hours of this
  window's log showed what they are: the stalls of the Event watch on k3s, which v0.5.1 still
  has, and the expired resource versions that end them, which v0.5.1 also counts ([2026-11-18 —
  The soak sampler on macOS](../journal/2026-11-18-the-soak-sampler-on-macos.md), "The stalls of
  the Event watch"). Memory did not grow with them.
- **Thirteen incidents in the window**, all `D006` (probe failures) of the cluster's own
  workloads.
- **cAdvisor reported both of S2's inputs** on every read: the highest usage, and the OOM events,
  always 0. That the highest usage is the cgroup's peak on cgroup v2 stays a hypothesis; it was
  1.8 times the highest working set sampled on the leader and 2.5 times on the standby.

## Limits

- **Dry-run.** No model call, no Event written, no notification: how the call budget, the queue
  and the breaker behave over days is not answered, and neither is the memory of the explanation
  path over days. A short run with explanations is in [Memory and CPU with
  explanations](2026-11-09-resource-use-with-explanations.md); a soak with the fake endpoint and
  the breaker exercises, as the steps of [Run the soak](../how-to/run-the-soak.md) describe, has
  not been run.
- **v0.5.1, not the latest release.** v0.6.3 came out during the window. Among its changes, a
  watch that ends with an error now lists again, so on this cluster it lists Events more often
  than v0.5.1 did; its memory over days was not measured.
- **A quiet, small cluster.** About 100 Pods, 13 incidents in a week and no seeded failures. The
  peak with thousands of Pods is the scale gate's question ([The scale
  gate](2026-11-24-the-scale-gate.md)).
- **5.7% of the window was not sampled**, nearly all of it in one gap. The working set in those
  hours is not known; no restart or OOM kill happened in them, since the restart counts did not
  change.
- **One window, on one cluster.** The sampler's output stays out of the repository; only these
  aggregates are published.
