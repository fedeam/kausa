# Run the soak

Run the pinned latest release for seven days on a small cluster, sample its memory, CPU and
metrics every 30 seconds, and decide with a rule fixed in advance whether the chart's memory limit
may go back from `192Mi` to `128Mi`.

Status as of 2026-11-03: the scripts in `tests/soak/` are written and were run once, for nine
minutes, in a throwaway kind cluster; no soak has been run yet, so there is no result. The release
is v0.5.1, the one pinned in `tests/e2e/upgrade-from.txt`.

## What the soak answers, and what it cannot

- **It answers** whether kausa's memory stays flat over days on a small ARM cluster, with real
  watch churn (the watch errors of k3s, [Memory and CPU on a Raspberry
  Pi 4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md)), the explanation path, the
  standby and the breaker exercised: the steady and the highest working set, CPU, restarts, and
  how the budget, queue and breaker gauges behave at the chart's defaults.
- **It cannot answer alone** the peak at scale. The `192Mi` limit exists for 10,000
  unschedulable Pods at the largest scheduling record ([List
  pages](../research/2026-11-02-list-pages.md)); a three-node cluster with about 100 Pods never
  gets near that. The decision rule below therefore has two gates: the soak, and a scale peak
  measured in kind.

## What it installs

`tests/soak/install.sh` installs, in the namespace `kausa-soak` (under the restricted Pod
Security Standard):

- the release `kausa-soak` of the pinned chart, downloaded from its GitHub release and checked
  against the pinned SHA-256, with the pinned image digest and `tests/soak/values.yaml`: two
  replicas, `rbac.scope=cluster` (the whole cluster's Pods and Events, the representative memory
  case), Events and the history off, notifications on, and every other value at the chart's
  default;
- a fake model endpoint, `kausa-soak-endpoint` (`tests/e2e/fake_endpoint.py` in a
  Deployment and a Service, `tests/soak/endpoint.yaml`), so that kausa really calls a model and
  the budget, queue and breaker gauges exist, which they do not in dry-run. The endpoint answers
  at once and logs nothing: the redacted contexts never leave the cluster and are not kept;
- the Secret `kausa-soak-notify`, with the endpoint's `/hook/` URL for the notifications.

kausa writes nothing outside `kausa-soak` but its ClusterRole and ClusterRoleBinding
`kausa-soak`: no Event and no Explanation on the cluster's workloads, and no
CustomResourceDefinition.

## Before you start

- A test cluster whose owner agreed to the soak and to the seeded failures. The seeded failures
  (`tests/bench/seeded-failures.yaml`) create crash loops, pull errors and quota refusals, two
  namespaces and a cluster-scoped RuntimeClass: apply them only with the cluster owner's consent,
  and never to a cluster that runs anything you care about.
- metrics-server in the cluster (without it, the `top_memory` and `top_cpu` fields are null), and
  about 100 MiB of memory free on a node for kausa, the endpoint and the seeded failures.
- An always-on machine with `kubectl`, `helm`, `curl` and `python3`, a kube context for the
  cluster, and network access to github.com; the cluster pulls kausa's image from ghcr.io and
  python's from Docker Hub. On macOS, run the sampler under `caffeinate -i`, so that idle sleep
  does not pause it.
- A directory outside the repository for the output, for example `~/kausa-soak/<date>/`.

**The kube context is always an explicit argument.** Every script refuses a missing or empty
context, or one that starts with `-`, and one that is not a whole name of
`kubectl config get-contexts -o name`; then every `kubectl` call passes `--context` and every
`helm` call `--kube-context`. The scripts never change the current context and never read it,
never print the cluster's server address, and write node indexes instead of node names. Below,
`<kube-context>` is the name of the test cluster's context.

## Steps

1. **Install** the endpoint and the release. It refuses when `kausa-soak` already exists, waits
   for two Ready kausa Pods and a holder of the Lease `kausa-soak-leader`, and prints the next
   commands:

   ```sh
   tests/soak/install.sh <kube-context>
   ```

2. **Apply the seeded failures**, with the cluster owner's consent. With `dedupHours` at 6, they
   give continuous churn and a model call per incident every 6 hours:

   ```sh
   kubectl --context <kube-context> apply -f tests/bench/seeded-failures.yaml
   ```

3. **Start the sampler** for seven days, on the always-on machine. It appends one JSON line per
   kausa Pod every 30 seconds to `<out>/samples-<UTC date>.jsonl` (the kubelet's working set and
   CPU, metrics-server, cAdvisor's highest usage and OOM events every 600 seconds, the Lease
   holder, restarts, and every `kausa_` series), and the cluster's counts to
   `<out>/cluster-<UTC date>.jsonl` every 600 seconds. A failed read is null and named in
   `errors`, never 0. Stopping it and starting it again appends to the same files; gaps are
   allowed up to the coverage rule below:

   ```sh
   caffeinate -i python3 -I tests/soak/sample.py --context <kube-context> --out <out> --days 7
   ```

   `--interval` sets the seconds between samples (30 by default).

4. **Open the breaker once a day** on days 2 to 6, at different hours. `exercise.sh` makes the
   fake endpoint answer 401 for the given minutes (10 by default, 1 to 60), creates a failing Job
   `soak-breaker-<UTC yyyymmddHHMM>` in `kausa-soak` so that a new incident calls the model, and
   then makes the endpoint answer again, also when it is interrupted:

   ```sh
   tests/soak/exercise.sh <kube-context> [minutes]
   ```

   Do nothing else to kausa during the soak: no restart, rollout or takeover, which would reset
   the process the growth test follows.

5. **Summarize** after 168 hours. `summarize.py` reads only the files: per Pod, the coverage,
   restarts, working set, cAdvisor's highest usage, the growth slope and CPU; per day, the
   increase of kausa's counters; the queue, the budget and the minutes with the breaker open; and
   then the three verdict lines of the rule below (`--hours` is 168 by default; 336 when the
   soak is extended):

   ```sh
   python3 -I tests/soak/summarize.py <out>
   python3 -I tests/soak/summarize.py <out> --hours 336
   ```

   `python3 -I tests/soak/summarize.py --self-test` checks the verdicts on synthetic samples
   without any file.

6. **Uninstall** the release and the namespace:

   ```sh
   tests/soak/uninstall.sh <kube-context>
   ```

   It does not remove the seeded failures. With the cluster owner's consent, delete what the
   header of `tests/bench/seeded-failures.yaml` lists:

   ```sh
   kubectl --context <kube-context> delete namespace kausa-test kausa-test-cron
   kubectl --context <kube-context> delete runtimeclass kausa-test-no-handler
   ```

**The output stays out of the repository.** It holds Pod names and the cluster's object counts.
Only the aggregates (the summary's numbers and the verdicts) go into a research page in
`docs/research/`.

## The decision rule

The rule was set on 2026-11-03, before any measurement, and is quoted here so that it cannot move
once numbers exist:

> The chart's memory limit goes back to `128Mi` in v0.6 **only if every gate below passes**. Any
> FAIL keeps `192Mi`, and the numbers are recorded in a research page either way. MiB = 2^20
> bytes.
>
> **Soak gate (a small ARM cluster, v0.5.1, the steps above):**
>
> - **S1 — validity** (an INVALID soak decides nothing; run it again): coverage at least 90% of
>   the 30-s slots for both Pods over the 168 hours; the same two Pod names throughout,
>   `restarts` 0; the breaker seen open (`kausa_endpoint_state{state="open"} 1` on the leader) on
>   at least 4 of the 5 exercise days; `kausa_model_calls_total{outcome="ok"}` increased on at
>   least 6 of the 7 days; `kausa_explanation_queue_length` above 0 in at least one sample. A
>   restart caused outside kausa (node reboot, eviction) restarts the 168-hour clock.
> - **S2 — no OOM and a small working set:** no `OOMKilled` and no `oom_events` increase; the
>   highest `ws_bytes` of each Pod is at most 32 MiB (the chart's request, a quarter of
>   `128Mi`); the highest `max_usage_bytes`, when cAdvisor reports it, is at most 64 MiB (half of
>   `128Mi`).
> - **S3 — no growth:** for each Pod, the least-squares slope of its hourly median `ws_bytes`
>   over hours 24-168 is at most 0.1 MiB per day, **and** the highest hourly median of the last
>   24 hours is at most 1 MiB above the highest of hours 24-48. A slope above 0.1 and at most
>   0.5 MiB per day is inconclusive: extend to 14 days and apply the same test to hours 24-336.
>   Above 0.5 MiB per day is a FAIL and an issue ("memory grows over days").
> - CPU, watch errors and the gauges are reported, not gated. A 30-s CPU p99 above 100m (half
>   the limit) is flagged in the research page.
>
> **Scale gate (kind, run after the soak gate passes):** the chart's Pod, the v0.5.1 image (or
> the v0.6 release candidate if `LIST_PAGE_SIZE` or the records changed), `rbac.scope=cluster`,
> `dryRun=true`, rollouts on, `resources.limits.memory=512Mi` so that the true peak shows,
> otherwise defaults; three starts (the install and two `kubectl rollout restart`s); each start's
> `memory.peak` of the container's cgroup read inside the kind node 3 minutes after Ready, as
> [Thirty-two namespaces](../research/2026-10-28-32-namespaces.md) did.
>
> - **C1:** with 10,000 unschedulable Pods at the largest scheduling record
>   (`python3 tests/bench/thousands-of-pods.py --unschedulable 20 50 10`, created with
>   `kubectl create`, as in [List pages](../research/2026-11-02-list-pages.md)), the highest
>   `memory.peak` of the three starts is at most 96 MiB (75% of `128Mi`, the margin rule of the
>   same page).
> - **C2:** with, in addition, the forged page (`python3 tests/bench/thousands-of-pods.py
>   --forged 500`), the highest `memory.peak` is below 128 MiB: no OOM at the new limit. The 75%
>   margin is not asked here: the `192Mi` limit does not meet it either.
>
> Outcomes: S1-S3, C1 and C2 pass → lower to `128Mi`. S1 INVALID → repeat the soak. Any FAIL →
> keep `192Mi`, record why. The scale gate is skipped (and recorded as not run) when the soak
> gate fails.

`summarize.py` prints S1 as `VALID` or `INVALID`, S2 as `PASS` or `FAIL`, and S3 as `PASS`,
`FAIL` or `INCONCLUSIVE` (its reason then ends with "extend to 14 days"); its thresholds are named
constants with these values. It does not run the scale gate.

Two of S2's inputs are hypotheses until the soak reads them: that the cluster's kubelet serves
cAdvisor's `container_memory_max_usage_bytes` and `container_oom_events_total`, and that on
cgroup v2 the first is the cgroup's peak. When cAdvisor does not report them, they are null and S2
rests on `ws_bytes` and the Pod's termination reason.
