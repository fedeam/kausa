# Thousands of Pods

How kausa starts against a cluster with thousands of Pods: how long until it is ready, how many
owner lookups it makes, whether its loop keeps answering the probes meanwhile, and its memory.
Measured on 2026-10-28 for the issue "run: look up owners only for Pods that fail", which asked
whether resolving the owner of every Pod could block the loop on a large cluster.

Once, in a kind cluster on the maintainer's machine: a first data point, not a benchmark.

## Result

| | 3,000 Pods, 1,000 ReplicaSets | 10,000 Pods, 1,000 ReplicaSets |
|---|---|---|
| Ready (`/readyz` 200) after start | 1.6 s | 1.6 s |
| Slowest `/healthz` answer while starting | 1.8 ms | 1.7 ms |
| ReplicaSet lookups | 1,002 | 1,002 |
| Memory, macOS binary (resident) | 36 MiB | 59 MiB |
| Memory, Linux image in a container (`docker stats`) | not measured | 31 MiB |

- **The loop is not blocked.** Owner lookups run inside the loop, but between one Pod and the
  next the loop answers the probes, so a probe waits at most for one lookup (which times out
  after 5 seconds), never for all of them. The slowest `/healthz` answer while 1,000 lookups ran
  was under 2 ms.
- **Lookups grow with the ReplicaSets and Jobs, not with the Pods**: one per ReplicaSet, cached,
  so ten times the Pods cost no more lookups. They happen once, when kausa starts or a new
  ReplicaSet appears.
- **Memory grows with the Pods**, by one small record each: about 2 MiB with 96 Pods on a
  Raspberry Pi 4 ([Memory and CPU on a Raspberry Pi
  4](2026-10-26-resource-use-on-a-raspberry-pi.md)), 31 MiB with 10,000 Pods in the Linux image.
  The chart's memory limit, 128 MiB, leaves room for several times that.

The change the issue proposed (looking up owners only for failing Pods) would save the lookups
at start, about a thousand `GET`s here, but the count of a workload's Pods that kausa sends with
an incident would then miss the healthy Pods of an older ReplicaSet during a rollout. It is not
worth it; the issue was closed with these numbers.

## Setup

- kind with Kubernetes v1.36.4 (the node image pinned in `.github/workflows/e2e.yml`).
- A fake node: a Node object with no kubelet, and Deployments whose Pods are bound to it with
  `nodeName` and tolerate everything, so that the API server holds thousands of real Pods and
  ReplicaSets and no container runs. 20 namespaces of 50 Deployments, with 3 and then 10
  replicas.
- kausa 0.1.0-dev at commit `4e8b64f`, release build, without an endpoint, through a
  `kubectl proxy` whose log counted the ReplicaSet requests; `/healthz` polled every 50 ms from
  start until `/readyz` answered 200.
- The Linux figure: the `runtime` image on kind's Docker network, read with `docker stats` 20
  seconds after start.
- The scripts: [`tests/bench/thousands-of-pods.py`](../../tests/bench/thousands-of-pods.py) writes
  the fake node and the Deployments, and
  [`tests/bench/measure-start.sh`](../../tests/bench/measure-start.sh) runs kausa and reports.

## Limits

- One machine and a local API server, which answers a lookup in about a millisecond. On a busy
  API server each lookup is slower and kausa is ready later, but the loop still answers between
  lookups.
- The Pods never run, so they produce no failures: this measures the cost of watching, not of
  explaining.
- The Linux memory at 3,000 Pods was not measured: Pods bound to a node without a kubelet cannot
  be deleted, so the cluster could not be scaled back down.
