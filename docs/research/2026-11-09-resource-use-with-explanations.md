# Memory and CPU with explanations

The first measurement on a Raspberry Pi 4 ([Memory and CPU on a Raspberry Pi
4](2026-10-26-resource-use-on-a-raspberry-pi.md)) ran in dry-run and left the explanation path
out: the model call, the validation and redaction of the answer, and the Event written per
incident. This one runs two releases of the published v0.6.1 image side by side on the same node,
one that explains and one in dry-run, against the same seeded failures, and compares them.

Measured on 2026-11-09, once, on one cluster: a data point, not a benchmark.

## Result

| | Explaining | Dry-run |
|---|---|---|
| CPU time, idle (10 minutes) | 0.07 s (0.1m) | 0.06 s (0.1m) |
| CPU time, with the failures (30 minutes) | 1.33 s (0.75m on average) | 1.19 s (0.67m on average) |
| Highest 30-second CPU average | 7.6m | 5.9m |
| Resident memory, idle | 0.68 MiB | 0.59 MiB |
| Resident memory, highest with the failures | 4.41 MiB | 3.25 MiB |
| Working set, highest with the failures | 5.29 MiB | 3.95 MiB |

- Both releases found the same 21 incidents across 14 detections (`D001` to `D014`): 20 in the
  first two minutes, and a second one for the CronJob that cannot create its Jobs at minute 22.
  The explaining release made 20 model calls, all answered and valid, reused the CronJob's
  explanation for its second incident (`dedupHours`), wrote 21 explanations as Events and
  redacted the answer's injected address, token and host in each one; the dry-run release logged
  21 requests.
- The explanation path cost about 1.2 MiB of resident memory and 0.14 s of CPU time over the 30
  minutes. Most of the memory, in both releases, is the record of the seeded Pods and Warning
  Events: resident memory went from under 1 MiB to 3 to 4 MiB in both when the failures appeared.
- The chart's requests (10m CPU, 32 MiB) stay well above both releases.

## Setup

- The cluster: Kubernetes v1.36.5 (k3s), three Raspberry Pi 4 Model B (8 GB) nodes on Debian 13,
  wired Gigabit Ethernet.
- `ghcr.io/fedeam/kausa:0.6.1`, installed twice with `charts/kausa` of v0.6.1 in one namespace
  labelled `pod-security.kubernetes.io/enforce=restricted`, both pinned to the same node:
  - **explaining**: `rbac.scope=namespaced` on the two namespaces of the seeded failures,
    `events=true`, `history.enabled=false`, and the model at the fake OpenAI-compatible endpoint
    of the end-to-end test ([`tests/e2e/fake_endpoint.py`](../../tests/e2e/fake_endpoint.py)),
    served in the cluster over plain HTTP (`model.allowHttp=true`). It answers at once with a
    fixed explanation whose second step carries an address, a token and a host for kausa to
    redact.
  - **dry-run**: the same values with `dryRun=true` and `events=false`.
  - The other values were the defaults, among them the call budget (30 calls an hour, 200 a day).
- The kubelet's summary API (`/api/v1/nodes/<node>/proxy/stats/summary`) every 30 seconds for 40
  minutes: the container's total CPU time, working set and resident memory.
- At minute 10, [`tests/bench/seeded-failures.yaml`](../../tests/bench/seeded-failures.yaml) of
  v0.6.1: 52 Pods and about 375 Events in its two namespaces at the end.

## Observations

- **The call budget paced the explanations.** Eight incidents were explained in the first minute
  of the failures: the start burst of 3 calls plus the 5 that the budget refilled during the 10
  idle minutes, one every 2 minutes. The other 12 waited in the queue (`explanations waiting`,
  `the hourly call budget is spent`) and were explained one every 2 minutes, the last 24 minutes
  after the failures appeared. Their CPU and memory came in small steps, not all at once.
- **Event watch errors.** Each release counted two expired resource versions (`The
  resourceVersion for the provided watch is too old: Expired`), at the same second, each followed
  by a relist; and, in the last 7 minutes, 14 (explaining) and 15 (dry-run) errors with `etcd
  event received with PrevKv=nil`, the k3s error described in [Memory and CPU on a Raspberry Pi
  4](2026-10-26-resource-use-on-a-raspberry-pi.md). No incident was missed. v0.6.1 counts the
  expired resource versions in `kausa_watch_errors_total`; from the next release a `410` counts
  as a relist only (CHANGELOG, Unreleased).

## Limits

- A fake endpoint that answers at once. A real model keeps the request open for seconds to
  minutes, and its answer is longer and varies; the memory held during a slow call, and HTTPS,
  were not measured here. On Docker Desktop, with a local model, kausa's resident memory was 4.4
  MiB ([Tokens per incident compared with k8sgpt](2026-10-26-tokens-compared-with-k8sgpt.md)).
- A small set of failures in two namespaces. Memory with thousands of Pods is in [Thousands of
  Pods](2026-10-28-thousands-of-pods.md).
- One run of 40 minutes.
