# List pages

How the size of the list pages of kausa's three watches (Pods, Warning Events and Deployments)
changes its peak memory, its time to ready and the list requests it makes, at 10,000 Pods and with
a page of Deployments as large as the API server allows, for 500 (kube-runtime 4.2.0's default),
250, 100 and 50 objects per page. Measured on 2026-11-02 to choose `LIST_PAGE_SIZE` in
`src/run.rs`, the related work of [0021. Refill the hourly call budget at a steady rate from a
start burst of three, and let incidents wait for it in the queue, so that a restart does not refill
it](../adr/0021-refill-the-call-budget.md).

Once per setting (twice for 500 and 50 with the forged page), in a kind cluster on the
maintainer's machine: a first data point, not a benchmark.

## Result

| Objects per page | 500 | 250 | 100 | 50 |
|---|---|---|---|---|
| 10,000 Pods: peak memory, macOS binary (maximum resident) | 160.7 MiB | 151.3 MiB | 138.2 MiB | 130.0 MiB |
| 10,000 Pods: ready (`/readyz` 200) after start | 2.57 s | 2.75 s | 2.73 s | 2.80 s |
| 10,000 Pods: list requests (one per page, the three watches) | 56 | 110 | 274 | 553 |
| With 500 forged Deployments: peak memory | 608.3 MiB; 591.1 MiB | 411.6 MiB | 234.4 MiB | 161.2 MiB; 169.9 MiB |
| With 500 forged Deployments: ready after start | 3.85 s; 2.06 s | 3.19 s | 2.55 s | 3.40 s; 3.29 s |
| With 500 forged Deployments: list requests | 67; 70 | 132 | 330 | 665; 672 |
| Slowest `/healthz` answer while starting, all runs | under 49 ms | under 5 ms | under 9 ms | under 2 ms |

- **The page, not the records, sets the peak.** A list response is decoded whole before kausa
  reduces each object to its record. One page of the 500 forged Deployments was 129,289,440 bytes
  of JSON, and kausa's peak rose by about 450 MiB over the same cluster without them; with pages of
  50 (12,941,573 bytes) it rose by about 31 to 40 MiB. kausa keeps no record of a Deployment that
  is not stalled, so this memory is all transient, but a Pod's memory limit is a peak.
- **Smaller pages cost requests, not time.** 50 objects per page made ten times the list requests
  of 500 (553 against 56 for 10,000 Pods, about 1,000 Deployments and the Warning Events), and
  kausa was ready within 0.25 seconds of the run with 500 at 10,000 Pods; the forged runs
  spread from 2.06 to 3.85 seconds with no order. The requests are made when a watch lists:
  at start, at a takeover and after a watch has to relist.
- **Without forged objects, smaller pages help too.** At 10,000 Pods at the largest scheduling
  record, the peak went from 160.7 MiB with 500 to 130.0 MiB with 50.
- **The decision rule**, set before the measurement: the largest page whose peak with the forged
  page is at most 144 MiB (75% of the chart's memory limit of 192Mi; the macOS figure is taken as
  a conservative upper bound of the Linux one) and whose time to ready at 10,000 Pods is at most
  twice that of 500; if none meets it, the lowest peak, with a flag. **None met it**: the lowest
  peak with the forged page was 50, at 161.2 and 169.9 MiB, and its time to ready, 2.80 s, is
  within twice 2.57 s. `LIST_PAGE_SIZE` is 50.
- **Flag: the 75% margin does not hold with a forged page.** With pages of 50 the macOS peak
  stayed under 192 MiB but above 144 MiB; the Linux image in a container was not measured here,
  so whether the chart's Pod keeps its margin is a hypothesis (the 2026-10-29 note measured the
  Linux image at about 65% of the macOS resident memory, with no forged objects). With pages of 500
  the same cluster would most likely get kausa killed for memory at every start (hypothesis: not
  run in the chart's Pod).
- **A full queue of 16 at the largest record (an estimate from the code's caps, not measured).**
  A queued job copies the records of its incident's Pods (at most 100 while it is pending, plus
  any others failing when it settles, `src/incident.rs`) and of their Warning Events (a note of at
  most 1,024 bytes, `src/record.rs`). At about 6.4 KiB per Pod at the largest scheduling record
  ([Richer context, measured](2026-10-29-richer-context-measured.md)) and one event of about
  1.3 KiB per Pod, a job holds about 0.8 MiB and 16 jobs about 12 to 13 MiB, plus the one log read
  in flight (at most 1 MiB read, 32 KiB kept). Jobs now wait longer for the budget, so this is
  held for longer, not larger.

## Setup

- kind v0.33.0 with Kubernetes v1.36.4 (the node image pinned in `.github/workflows/e2e.yml`), one
  node, on an Apple silicon laptop; Docker's VM had 16 CPUs and 7.65 GiB of memory.
- kausa at commit `cf87a3d` (v0.4.1 with the v0.5 budget work, not yet merged), with
  `LIST_PAGE_SIZE` set to 500, 250, 100 and 50 in four local release builds, run on the laptop
  without an endpoint and with rollouts on (the default), through a `kubectl proxy`.
- 10,000 Pods: `python3 tests/bench/thousands-of-pods.py --unschedulable 20 50 10`, created with
  `kubectl create`: 1,000 Deployments of 10 Pods that no node can take, each Pod with the
  scheduling record at its caps.
- The forged page: `python3 tests/bench/thousands-of-pods.py --forged 500`, created with
  `kubectl create` after the Pods: 500 Deployments of zero replicas, each with a 256,000-byte
  annotation (the API server limits an object's annotations to 256 KiB). In the namespace
  `forged-00`, they sort before every other Deployment, so the first page of the cluster-wide list
  holds as many of them as the page allows.
- [`tests/bench/measure-start.sh`](../../tests/bench/measure-start.sh) reports the peak (the
  maximum resident set size of `/usr/bin/time -l`, from start until it stops kausa 3 seconds after
  ready), time to ready, and the list requests (the `GET`s with `limit=` in the proxy's log).

## Limits

- One run per setting, two for the ends; the two forged runs at 500 differ by 17 MiB and at 50 by
  9 MiB, so the steps between 250, 100 and 50 are an order of magnitude, not a curve.
- macOS resident memory, not the Linux image's working set or `memory.peak`; the cluster was not
  installed with the chart.
- A forged page is a constructed worst case: whoever can create Deployments can also create them
  near the 1.5 MiB object limit with other fields (hypothesis), so a page of 50 such objects can
  still decode to more than measured here.
- Each list response comes through `kubectl proxy` from a local API server; on a busy API server
  the ten times more list requests take longer.
