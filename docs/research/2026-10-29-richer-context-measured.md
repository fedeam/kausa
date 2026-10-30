# Richer context, measured

What the context of [0017. Send the model resource requests, probe handlers, image pull settings
and an unschedulable Pod's scheduling keys, reduced and redacted](../adr/0017-richer-context-for-the-model.md)
costs in tokens per incident and in memory with thousands of Pods: the token benchmark and the
scale measurement again, with an HTTP probe and an unschedulable Deployment at the largest
scheduling record kausa keeps, each run with kausa before 0017 and with kausa now, against the
same cluster. Measured on 2026-10-29 for 0017's "Before acceptance".

Once per setting (twice for the tokens), in a kind cluster on the maintainer's machine: a first
data point, not a benchmark.

## Result

| | Before 0017 | With 0017 | Change |
|---|---|---|---|
| The five workloads of the 2026-10-26 benchmark: request bytes | 12,599 | 14,595 | +1,996 (+16%) |
| The same five: input tokens | 2,495 | 3,018 | +523 (+21%) |
| The same five: output tokens | 828 | 703 | -125 (the model's, run to run) |
| `D006` with an HTTP readiness probe: bytes, input tokens | 2,687, 553 | 3,012, 626 | +325, +73 |
| `D005` at the largest scheduling record: bytes, input tokens | 2,579, 525 | 8,829, 1,928 | +6,250, +1,403 |
| 10,000 unschedulable Pods at the largest record: memory, macOS binary (resident) | 117 MiB | 180 MiB | +63 MiB |
| The same, Linux image in a container (`docker stats`), rollouts off; on | not measured | 117 MiB; 111 MiB | |
| The same, the chart's Pod with its defaults (memory limit 128 MiB) | not measured | `OOMKilled` at start, every restart | |
| The same, the chart's Pod with a 512 MiB limit: steady (working set), peak (`memory.peak`) | not measured | 112 to 116 MiB, 138.8 MiB | |
| 5,000 such Pods, the chart's Pod with its defaults: steady, peak | not measured | 60 to 62 MiB, 85.5 MiB, no restart | |
| 3,000 Pods bound to a node, three HTTP probes each: memory, macOS binary (resident) | 48.6 MiB | 46.8 MiB | none measurable |
| The same, Linux image in a container (`docker stats`) | not measured | 21.3 MiB | |
| ReplicaSet lookups; ready after start; slowest `/healthz` while starting (10,000 unschedulable) | 1,002; 2.2 s; 7 ms | 1,002; 2.5 s; 7 ms | none |
| The same (3,000 bound) | 1,002; 1.2 s; 1.6 ms | 1,002; 1.4 s; 0.4 ms | none |

- **Every request grew by 196 bytes** (about 41 tokens with this model), the longer system prompt
  that names the new fields (`PROMPT_VERSION` `"2"`): `D008`, `D011`, `D012` and `D013`, which
  carry no Pod, grew by exactly that.
- **The new fields add little to the usual incidents:** requests per container, a probe's
  handler, port and path, and for `D002` the pull policy and the count of pull secrets: 310 to 570
  bytes and 68 to 182 tokens per incident, the prompt included (`D001` and `D003` also differ by
  their log tails). `D005` for `toobig`, which sets no selector, carries the two tolerations every
  Pod gets (fact 2 of 0017).
- **The largest scheduling record is the one large field:** 32 keys of 128 bytes, the caps of
  `src/record.rs`, add 5.7 KB to the request (6,250 bytes with the prompt and the redacted keys),
  1,403 tokens, and still fit the default `--max-input-tokens` of 6,000. The two default
  tolerations were counted in `omitted`.
- **Memory: 0017's hypothesis does not hold at its worst case.** 0017 expected at most about 6 KiB
  per unschedulable Pod, "about 57 MiB for 10,000 such Pods at that worst", and, as a hypothesis,
  "within the margin of Thousands of Pods". The resident memory grew by 63 MiB for 10,000 such Pods (about 6.4 KiB
  each, close to the estimate), but that is not within the margin: kausa, installed with the
  chart's defaults among them, was killed for memory a few seconds after each start (its last
  line was an incident, so it had listed); with the limit raised to 512 MiB it stayed at 112 to
  116 MiB and its peak was 138.8 MiB, above the default limit of 128 MiB. With half as many,
  5,000, the chart's defaults held: no restart in four minutes, 60 to 62 MiB steady and a peak of
  85.5 MiB. Where between 5,000 and 10,000 such Pods the default limit is crossed was not
  measured. Pods this large are a constructed worst case, but whether the chart's limit or the
  record's caps change is a decision left open (0017's `Update` of the same day).
- **Probes and requests on ordinary Pods cost nothing measurable:** with 3,000 Pods, each with
  three 128-byte probe paths and four quantities, the resident memory was 48.6 MiB before and
  46.8 MiB after (and 55.8 MiB with 0018's Deployment watch on), within the run-to-run spread of
  the macOS figure; the Linux image used 21.3 MiB. A healthy Pod keeps no scheduling record.
- **The owner lookups and readiness do not change:** 1,002 ReplicaSet lookups for 1,000
  ReplicaSets in both setups, ready within 1.2 to 2.9 seconds, and the slowest `/healthz` answer
  while starting under 8 ms, before and after 0017.
- **Against the earlier notes.** [Tokens per incident compared with
  k8sgpt](2026-10-26-tokens-compared-with-k8sgpt.md) counted 2,183 input and 765 output tokens
  for the five workloads on a k3s cluster with an older kausa; the run before 0017 here counted
  2,495 and 828 on kind, so the cluster and the versions between them moved the baseline too, and
  the change above is read against the run of the same day. k8sgpt was not run again: 0017
  changes nothing on its side. [Thousands of Pods](2026-10-28-thousands-of-pods.md) measured
  Pods with no probes and no requests, bound to a node; the rows above at 3,000 Pods repeat it
  with both.

## Setup

- kind v0.33.0 with Kubernetes v1.36.4 (the node image pinned in `.github/workflows/e2e.yml`), one
  node, on an Apple silicon laptop; Docker's VM had 16 CPUs and 7.65 GiB of memory.
- kausa before 0017: commit `15bd9a7`, the parent of 0017's implementation. kausa with 0017:
  commit `dcff499` (0017, 0018 and 0019's implementation, not yet merged). Both release builds,
  run on the laptop through a `kubectl proxy`; the one with 0017 with `--no-rollouts`, so that
  0018's Deployment watch, which the older one does not have, is not counted (one row also with
  it on).
- **Tokens:** the method of [Tokens per incident compared with
  k8sgpt](2026-10-26-tokens-compared-with-k8sgpt.md): Ollama 0.9.2 with `qwen3:0.6b` and
  `OLLAMA_NUM_PARALLEL=1`, [`tests/bench/token_proxy.py`](../../tests/bench/token_proxy.py) in
  front of it, kausa with `--endpoint`, `--no-events`, the default budget, through the read-only
  proxy of [Try kausa against a cluster with a local model](../how-to/try-with-a-local-model.md).
  [`tests/bench/seeded-failures.yaml`](../../tests/bench/seeded-failures.yaml) gained two
  Deployments at its end, the earlier objects unchanged: `unready`, an HTTP readiness probe
  (`/ready`) on a port nothing listens on (`D006`), and `unplaceable`, an unschedulable Pod with
  8 node selector keys, 8 required node affinity requirements, 4 required Pod affinity and 4
  anti-affinity terms and 8 tolerations, every key 128 bytes long (`D005`). Each binary ran
  until it had explained every incident in `kausa-test`, then the other, twice. Which call was
  which incident comes from kausa's log, where the explanations are written in the order of the
  calls. A `--dry-run` of each binary gave the request bodies, to tell the system prompt from the
  context.
- **Scale:** [`tests/bench/thousands-of-pods.py`](../../tests/bench/thousands-of-pods.py) gained
  two options. `--unschedulable` writes Deployments whose Pods no node can take, with the
  scheduling fields at the caps of `src/record.rs` as above; `--probes` gives each container
  requests and limits and an HTTP liveness, readiness and startup probe whose paths are 128
  bytes, the longest kept. First `--unschedulable --probes 20 50 10`: 10,000 Pods, each Pending
  with `PodScheduled` `False` and reason `Unschedulable` (checked), about 9 KB each as JSON, and
  10,000 `FailedScheduling` events. Then those Deployments scaled to 5 replicas, and then
  `--probes 20 50 3`, 3,000 Pods bound to the fake node, as in the earlier note.
  [`tests/bench/measure-start.sh`](../../tests/bench/measure-start.sh) as before for the macOS
  rows; the Linux image (`runtime` target) on kind's Docker network, read with `docker stats` 20
  seconds after ready; and the chart, `rbac.scope=cluster`, `dryRun=true`, otherwise its defaults
  (128 MiB memory limit, `rollouts` on), with the container's cgroup read on the node every 15
  seconds for three to four minutes (working set as the kubelet counts it, and `memory.peak`).

## Limits

- One machine and a local API server; one model, a very small one, whose output varies from run
  to run (the output tokens above are not 0017's doing). Input tokens repeated exactly between the
  two runs except where a log tail is sent (`D001`, `D003`), which differs with each restart. In
  the second runs the failed Job's event was too old for `D008`, so nine incidents were explained;
  the table uses the first runs.
- The laptop was not kept awake during these runs, and its idle sleep paused Docker's VM at times
  (found later the same day, see [Standby and failover](2026-10-29-standby-and-failover.md)). The
  token and memory figures do not depend on it; the times to ready look normal, but a pause inside
  one cannot be excluded.
- The 10,000 unschedulable Pods are a constructed worst case: every key at 128 bytes and every
  list at its cap. They also bring 10,000 Warning events, which both versions keep. The memory
  before 0017 was measured only with the macOS binary: no image of the older commit was built,
  for the shared disk.
- The resident memory of the macOS binary is higher than the Linux image's for the same objects,
  as in the earlier note; the chart's limit applies to the Linux one.
