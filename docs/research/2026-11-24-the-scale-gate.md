# The scale gate

The scale gate of [Run the soak](../how-to/run-the-soak.md#the-decision-rule) was run on
2026-11-24, after the soak gate passed: with 10,000 unschedulable Pods the chart's Pod peaked at
119.6 MiB, above the 96 MiB the rule allows, so C1 fails and the chart's memory limit stays at
`192Mi`. With the forged page as well the peak stayed below 128 MiB, so C2 passes.

Three starts for each gate and each of two releases, in one kind cluster on the maintainer's
machine: a first data point, not a benchmark.

## Result

The highest memory since the container started (`memory.peak` of the container's cgroup, read
inside the kind node 3 minutes after the Pod was Ready), in MiB, for each of the three starts:

| | v0.5.1, the rule's image | v0.6.3 | The rule |
|---|---|---|---|
| C1: 10,000 unschedulable Pods | 119.6, 115.7, 116.5 | 118.1, 118.0, 118.5 | highest at most 96 MiB |
| C1, verdict | **FAIL** (119.6) | FAIL (118.5) | |
| C2: the same, with 500 forged Deployments | 125.3, 125.1, 124.4 | 127.9, 121.8, 124.1 | highest below 128 MiB |
| C2, verdict | **PASS** (125.3) | PASS (127.9) | |
| Memory in use at the same reading (`memory.current`), C1 | 103.2, 99.3, 100.4 | 99.8, 100.4, 101.1 | not gated |
| Memory in use at the same reading, C2 | 107.7, 103.5, 105.3 | 108.2, 105.9, 105.1 | not gated |
| OOM kills (`memory.events`), restarts, the twelve starts | 0, 0 | 0, 0 | |

- **What it decides.** The rule's outcomes are "S1-S3, C1 and C2 pass → lower to `128Mi`" and
  "Any FAIL → keep `192Mi`, record why". The soak gate passed ([Seven days of v0.5.1 in
  dry-run](2026-11-23-seven-days-in-dry-run.md)) and C2 passes, but C1 fails: the chart's limit
  stays at `192Mi`, and nothing in the chart changes.
- **Why.** At 10,000 Pods at the largest scheduling record the chart's Pod needs 116 to 120 MiB
  at its peak, 90% to 93% of `128Mi`, where the margin rule asks for at most 75%. The gate was
  not lost by a spike: three minutes after Ready the container still held 99 to 103 MiB, itself
  above the 96 MiB bar.
- **The image the rule names is v0.5.1.** `LIST_PAGE_SIZE` is 50 in v0.5.1 and in v0.6.3, and
  `src/record.rs` is the same in both, so the condition that would have moved the gate to a
  later release does not hold. v0.6.3, the latest release, was measured beside it for the
  record, and it gives the same verdicts.
- **C2 passes without room.** The highest peak with the forged page was 125.3 MiB with v0.5.1
  and 127.9 MiB with v0.6.3: 134,144,000 bytes, 73,728 under 128 MiB. The rule asks only for no
  OOM kill at `128Mi`, and a limit that a start comes that close to is no margin.
- **`192Mi` meets the 75% margin in the chart's Pod.** The rule's text says that the `192Mi`
  limit does not meet the margin with the forged page; that came from the macOS binary's 161 to
  170 MiB ([List pages](2026-11-02-list-pages.md)). In the chart's Pod the highest peak of the
  twelve starts, 127.9 MiB, is 67% of `192Mi`, under the 144 MiB of the margin.

## Setup

- kind v0.33.0 with Kubernetes v1.36.4 (the node image pinned in `.github/workflows/e2e.yml`), one
  node, on an Apple silicon laptop; Docker's VM had 16 CPUs and 7.65 GiB of memory, cgroup v2.
- The published images, pulled by the node: v0.5.1,
  `ghcr.io/fedeam/kausa@sha256:9c1dc28d815b579e493817cb6ea817a4ac3bbca1ab85f7b470a2ecbf05027b91`,
  and v0.6.3,
  `ghcr.io/fedeam/kausa@sha256:6f812c35c7514db9f56bfbc971739bf765deb30e4f876143dfb5257928176eb6`,
  as the kubelet reported them.
- Each release with the chart of its own tag, installed with Helm: `rbac.scope=cluster`,
  `dryRun=true`, `resources.limits.memory=512Mi`, otherwise defaults (one replica, rollouts on,
  no endpoint). kausa's `started` line had `rollouts: true` in the four logs kept.
- Three starts: the install, then two `kubectl rollout restart`s. The chart's strategy is
  `Recreate`, so no two Pods of kausa ran at once. The release was uninstalled before the next
  one was installed.
- C1: `python3 tests/bench/thousands-of-pods.py --unschedulable 20 50 10`, created with
  `kubectl create`: 1,000 Deployments of 10 Pods that no node can take, each Pod with the
  scheduling record at its caps. All 10,000 Pods were Pending before the first install and
  after the last start.
- C2: in addition, `python3 tests/bench/thousands-of-pods.py --forged 500`, created with
  `kubectl create` after the Pods: 500 Deployments of zero replicas, each with a 256,000-byte
  annotation, in a namespace that sorts before every other.
- The reading: 180 seconds after the Pod's Ready condition, `memory.peak`, `memory.current` and
  the `oom_kill` count of `memory.events` of the container's cgroup, found from the container's
  process inside the kind node, as [Thirty-two namespaces](2026-10-28-32-namespaces.md) did.

## Observations

- **The memory is the process's own.** One more reading of a v0.5.1 start of C1 broke the
  cgroup's memory down: 95.5 MiB anonymous, 1.1 MiB of file pages and 0.4 MiB of kernel memory,
  out of 98.7 MiB in use.
- **The forged page cost less here than on macOS.** It raised the highest peak by 5.7 MiB with
  v0.5.1 and by 9.5 MiB with v0.6.3, against the 31 to 40 MiB of the macOS binary with pages of
  50 in [List pages](2026-11-02-list-pages.md). Why was not looked into; the two measurements
  differ in the binary, the allocator and what the peak counts.
- **The Linux image peaks below the macOS binary without the forged page too:** 116 to 120 MiB
  against 130.0 MiB at 10,000 Pods with pages of 50.
- **kausa did its work at every start checked.** The log of the last start of each three has
  1,000 incidents, one per Deployment, of which the dry-run logged 16 or 32 requests and dropped
  the rest because its queue was full.
- **The two releases are within the spread of the starts.** In C1, v0.5.1 spread over 3.8 MiB
  and v0.6.3 over 0.5 MiB; in C2, 0.8 MiB and 6.1 MiB. Nothing here says that one release uses
  more than the other.

## Limits

- Three starts for each cell, in one cluster on one machine, once. The spread between starts,
  up to 6.1 MiB, is larger than the 0.07 MiB by which v0.6.3 passes C2.
- kind on a laptop, not a Raspberry Pi and not a real control plane: the API server is local and
  the 10,000 Pods are Pending, so no kubelet reports on them.
- Dry-run: no model call, no Event written. What the explanation path adds at this scale was
  not measured; [Memory and CPU with explanations](2026-11-09-resource-use-with-explanations.md)
  measured it on a small cluster.
- The forged page is a constructed worst case, and not the largest one: objects nearer the
  object size limit can decode to more ([List pages](2026-11-02-list-pages.md)).
- What holds the 100 MiB three minutes after Ready (the records of 10,000 Pods, the pending
  incidents or memory the allocator has not returned) was not profiled.
- The VM was under memory pressure from the cluster itself: 1.3 GiB of its 7.65 GiB were
  available at the end. A cgroup's `memory.peak` counts what the container charged, so this
  should not lower it (hypothesis).
