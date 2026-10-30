# Standby and failover

What the standby replica of [0019. Offer an optional standby replica that takes over through a
chart-created Lease, elected by kausa's own code](../adr/0019-high-availability.md) costs and how
fast it takes over: the standby's memory, the takeover after a graceful delete and after a frozen
leader, five times each, the Lease calls per minute, and the leader's renewals and progress while
its metrics port is flooded at the chart's CPU limit of 200m and while it relists thousands of
Pods. Measured on 2026-10-29 as 0019's condition of shipping.

In a kind cluster on the maintainer's machine: five runs of each takeover, one of each flood; a
first data point, not a benchmark.

## Result

| | Measured |
|---|---|
| Standby's memory: working set; resident (`VmRSS`); peak (`memory.peak`) | 0.6 MiB; 4.4 MiB; 5.1 MiB |
| Leader's memory beside it, five incidents explained | 2.3 MiB; 7.3 MiB; 5.4 MiB |
| Standby's CPU | 0.07m (16 ms in 213 s) |
| Lease calls per minute, both replicas (5 minutes) | 23.8: 11.9 `patch` by the leader, 11.9 `get` by the standby, no error |
| Renewal delay past 5 s, idle (60 renewals): median, 95th percentile, max | 3 ms, 8 ms, 11 ms |
| Graceful delete of the leader, one node: takeover after the delete, five runs | 0.44, 0.46, 0.49, 0.51, 0.80 s (median 0.49 s), each time by the **replacement** Pod |
| Graceful delete with the node cordoned (the replacement stays `Pending`): takeover by the standby | 1.49, 1.51, 1.90, 2.14, 4.18 s (median 1.90 s) |
| The release, after the delete | 0.03 to 0.09 s, in all ten runs |
| Frozen leader (`SIGSTOP`): takeover by the standby after the stop, five runs | 27.72, 27.74, 27.79, 29.89, 32.70 s (median 27.79 s) |
| The same, after the frozen leader's last renewal | 30.04, 30.13, 30.19, 31.45, 35.14 s (median 30.19 s) |
| New leader watching (its three lists done) after it took the Lease, small cluster | 0.04 to 0.08 s |
| Flood of 64 connections for 180 s: answers; leader's CPU; periods throttled | 974,263 (about 5,400 a second); 199m; 1,800 of about 1,810 |
| The same, 256 connections | 965,551; 199m; 1,801 |
| Renewal delay under the flood (36 renewals each): median, max | 64: 1 ms, 24 ms; 256: 1 ms, 20 ms |
| Failing Job created to its `Explained` Event: no flood, 64, 256 connections (three Jobs each) | 3.5 s, 7.5 s, 8.5 s |
| Leadership changes, restarts during the floods | none, none |
| With 10,000 Pods (20 namespaces, 1,000 Deployments): frozen leader, takeover after the stop; quiet, then with 64 connections flooding the standby | 26.56 s; 34.45 s |
| The new leader's watches open (10,000 Pods listed) after it took the Lease | 2.6 s; 4.5 s |
| Its renewal delay in its first minute (12 renewals): median, max | 2 ms, 64 ms; 1 ms, 165 ms |
| Its memory after the relist: working set, peak | 43.0 MiB, 57.1 MiB; 45.5 MiB, 58.8 MiB |

- **The standby costs almost nothing.** Under a mebibyte of working set and 4.4 MiB resident (the
  static binary's pages), about a tenth of a millicore, and 12 Lease reads a minute. With the
  leader's 12 renewals, 0.4 requests a second, as 0019 expected.
- **A graceful takeover is faster than 0019 expected, and usually not by the standby.** On one
  node, with the image present, the Deployment's replacement Pod started, found the Lease released
  and took it within 0.44 to 0.80 seconds of the delete, before the standby's next read: five times
  out of five. 0019's "about 5 s via the release" is the standby's path; with the node cordoned,
  so that the replacement could not start, the standby took over 1.5 to 4.2 seconds after the
  delete, within its 5-second read period. Either way the old leader released the Lease 30 to 90
  ms after the delete, and the new leader had its watches open within a tenth of a second.
- **A frozen leader is replaced 30 to 35 seconds after its last renewal.** From the stop, 27.7 to
  32.7 seconds. One run of five took 35.14 seconds from the last renewal, 0.14 seconds over 0019's
  "within 35 s (`LEASE_DURATION + RETRY_PERIOD`)". The audit log shows why: the standby first saw
  the renewed version 0.14 seconds after the renewal, and its sixth read after that came 29.999
  seconds later, a millisecond short of `LEASE_DURATION`, so it took the Lease at the seventh. The
  worst case is `LEASE_DURATION + 2 × RETRY_PERIOD` (40 s) after the last renewal, which is what
  the unit test of `src/leader.rs` already asserts; the e2e test's 45 seconds from the stop
  covers it. The flooded relist run below met the same boundary (35.14 s again). Every resumed
  leader logged `leadership lost` (`not renewed in time`) and `standby`, and none restarted.
- **A metrics-port flood does not delay the renewals.** At the 200m limit, throttled in nearly
  every 100 ms period for three minutes and answering about 5,400 requests a second, the leader
  renewed every 5 seconds with at most 24 ms of delay, the standby never took over, and the three
  failing Jobs seeded into each flood were explained, 4 to 5 seconds later than without it.
- **A relist of 10,000 Pods delays a renewal by tens of milliseconds.** The new leader listed
  the Pods in 2.6 seconds (4.5 under the flood), the 200m limit throttling it in 25 periods, and
  renewed on time throughout: at most 64 ms late, 165 ms with the flood, against 15 seconds of
  margin before `RENEW_DEADLINE`. The flooded standby kept reading the Lease every 5.0 seconds
  before it took over. Its memory after the relist, 43 to 46 MiB, is the leader's of a 10,000-Pod
  cluster; a leader that steps down drops its records (3.6 MiB working set afterwards, in the run
  that followed).
- **Not measured:** whether tokio's cooperative budget alone, without the nested select, would
  have left the controller a share is still a hypothesis, as 0019 says: only the built select was
  run.

## Setup

- kind v0.33.0 with Kubernetes v1.36.4 (the node image pinned in `.github/workflows/e2e.yml`), one
  node, on an Apple silicon laptop; Docker's VM had 16 CPUs and 7.65 GiB of memory. The API
  server wrote an audit log of every request by kausa's service account (`Metadata` level), which
  gave every Lease call, renewal and list with the API server's clock; the action of each run
  (the delete, the `SIGSTOP`) was timed on the same node's clock.
- kausa at commit `dcff499` (0019's implementation, PR #52 before its last fix to the e2e test),
  the `runtime` image, installed with the chart in the repository: `replicaCount=2`,
  `rbac.scope=cluster`, the default resources (200m CPU and 128 MiB limits, 10m and 32 MiB
  requests), the e2e test's fake model endpoint on the host, `settleSeconds=10`, and a budget of
  1,000 calls an hour so that it never stopped a call. The e2e failures (`tests/e2e/failures.yaml`,
  with this image) were seeded in `e2e-shop` and explained by the leader.
- **Memory and CPU:** the container's cgroup on the node (working set as the kubelet counts it,
  `memory.peak`, `cpu.stat`) every 30 seconds for three and a half minutes, and `VmRSS` from
  `/proc`.
- **Takeovers:** each run waited for two Ready Pods and a holder, then 12 seconds; then deleted the
  leader Pod with `--wait=false` from the node, or sent `SIGSTOP` to its process found on the node
  by its `POD_NAME`, as `tests/e2e/run.sh` does, and `SIGCONT` once the Lease named another Pod.
  The takeover is the API server's receipt of the first successful patch by another Pod; which
  Pod it was comes from its IP.
- **Flood:** a Pod running `curl` 8.11.1 with `--parallel --parallel-max 64` (then 256) and a URL
  range, so that every request was a new connection to `GET /metrics` on the leader's Pod IP, for
  180 seconds; 20 seconds in, three Jobs whose Pod fails at once (`D008`). The time to explained
  is from the Job's `creationTimestamp` (whole seconds) to the `eventTime` of kausa's Event, so it
  is good to about a second. The same without the flood first.
- **Relist:** [`tests/bench/thousands-of-pods.py`](../../tests/bench/thousands-of-pods.py)
  `--probes 20 50 10`, 10,000 Pods bound to a fake node, then a frozen leader as above, once
  quiet and once with the 64-connection flood started on the standby 15 seconds before the stop;
  the new leader's renewals for 60 seconds after it took the Lease. One run each.
- The host was kept awake (`caffeinate`) for every timed run: before it, the laptop's idle sleep
  paused Docker's VM and left gaps of minutes in the audit log, which no time above includes.

## Limits

- One node: the replacement Pod starts at once and wins graceful handovers. With two nodes and
  the preferred anti-affinity, or an image to pull, the standby's path (1.5 to 4.2 s) is the
  likelier one; that was not measured.
- A paused process stands in for a node that dies or is cut off; the API server stays up. An API
  server outage, a network partition and a real node failure were not measured.
- One flood shape, many short `GET /metrics` requests; connections that open and send nothing
  hold one of the 8 answer slots for 2 seconds each and were not measured.
- The laptop's API server answers in about a millisecond; renewal delays on a loaded API server
  were not measured.
