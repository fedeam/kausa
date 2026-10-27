# Memory and CPU on a Raspberry Pi 4

kausa's image, installed with its Helm chart on a Raspberry Pi 4 node, watched a small cluster
for 22 minutes: 10 with nothing failing, then 12 with five seeded failures. The roadmap's fourth
weekend asks for this measurement ([0001. Build a small, read-only explainer that writes one
redacted explanation per incident](../adr/0001-scope-and-positioning.md)).

Measured on 2026-10-26, once, on one cluster: a first data point, not a benchmark.

## Result

| | Idle (10 minutes) | With five incidents (12 minutes) |
|---|---|---|
| Memory, `kubectl top` (working set) | 1 MiB | 2 MiB |
| Memory at the end, kubelet summary | | 1.88 MiB resident, 2.12 MiB working set |
| CPU, `kubectl top` | 1m (14m in the first sample, at start) | 1m to 2m |
| CPU time over the 22 minutes, kubelet summary | | 0.67 seconds |

- kausa found the five incidents (`D001`, `D002`, `D003`, `D005` and `D008`, one per failing
  workload) and logged their five requests (dry-run). Nothing else in the cluster was failing.
- The chart's Pod was admitted under the restricted Pod Security Standard, became ready, and did
  not restart. This was the chart's first install on a real cluster.
- The chart's default requests (10m CPU, 32 MiB) and limits (200m, 128 MiB) are far above what
  was used; they stay as they are, as room for larger clusters and for the explanation path,
  which this run did not exercise (see Limits).

## Setup

- The cluster: Kubernetes v1.36.5 (k3s), three Raspberry Pi 4 Model B (8 GB) nodes on Debian 13,
  89 Pods at the start and 96 with the seeded failures, 49 Events, 3 of them Warnings.
- kausa 0.1.0-dev at commit `7970179`, the linux/arm64 `runtime` image built from the
  Dockerfile and loaded into one node's containerd (no image is published). Installed with
  `charts/kausa` in a namespace labelled `pod-security.kubernetes.io/enforce=restricted`, with
  `dryRun=true`, `image.pullPolicy=Never` and a `nodeSelector` for that node; the other values
  were the defaults.
- `kubectl top pod` every 30 seconds (metrics-server: the working set, rounded to MiB and
  millicores), and the kubelet's summary API at the end
  (`/api/v1/nodes/<node>/proxy/stats/summary`) for the exact bytes and the total CPU time.
- At minute 10, [`tests/bench/seeded-failures.yaml`](../../tests/bench/seeded-failures.yaml).

## Observations

- **Watch errors on Events.** The Event watch was restarted 17 times: twice with `The
  resourceVersion for the provided watch is too old` (expected; the watcher lists again) and 15
  times with `Internal error occurred: etcd event received with PrevKv=nil`, about Events of other
  namespaces. kausa logs each one as `event watch error` and the watcher retries with backoff; no
  incident was missed. Why the API server sends that error for kausa's field-selected watch on
  this k3s cluster is not known yet (a hypothesis: Events that expire or are deleted while a
  watch with a field selector is open). Whether each restart also costs a new list of the
  Warning events or only a new watch from the last resource version was not checked; an audit on
  2026-10-27 judged the second more likely, on the reading that kube-rs re-lists after `Expired`
  and not after other errors (not verified). Since 2026-10-27 the restarts are counted in `kausa_watch_errors_total`.

## Limits

- Dry-run: no model call, no Event written. The explanation path adds an HTTPS request with a
  body of a few KiB, the answer (at most 256 KiB), the validation and one Event per incident; its
  memory was not measured on the Pi. On macOS, with a local model, kausa's resident memory was 21
  MiB, and in a container on Docker Desktop 4.4 MiB ([Tokens per incident compared with
  k8sgpt](2026-10-26-tokens-compared-with-k8sgpt.md), and the journal of 2026-10-26).
- A small cluster. kausa keeps a small record per Pod and per Warning event, so memory grows with
  their number; a cluster with thousands of Pods was not measured.
- One run of 22 minutes; a longer run would show whether memory stays flat.
