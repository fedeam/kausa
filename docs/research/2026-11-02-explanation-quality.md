# Explanation quality of local models against known causes

How often two local models, `qwen3:0.6b` and `qwen3:4b`, and one hosted model,
`gemini-3.5-flash-lite`, name the known cause of 20 seeded
failures (at least one per detection `D001` to `D014`), graded by a fixed keyword rubric written
before any model ran; measured once each on 2026-11-02.

Once each, an order of magnitude: one dry-run, one answer per request and per model, on one
machine. A difference of one or two entries between models or rules is noise until it is
repeated.

## Result

Entries that pass every criterion (valid, cause, first step, confidence, no invented object), out
of 20, every one detected:

| Model | Valid | Cause | First step | Confidence | No invented object | All |
|---|---|---|---|---|---|---|
| `qwen3:0.6b` | 20 | 15 | 16 | 20 | 17 | **10** |
| `qwen3:4b` | 20 | 16 | 19 | 20 | 19 | **15** |
| `gemini-3.5-flash-lite` (hosted) | 19 | 18 | 17 | 19 | 19 | **16** |

- Both local models answered valid JSON every time, with the request's own `response_format`.
- `gemini-3.5-flash-lite` (Google's OpenAI-compatible endpoint, free tier, the same requests)
  passed every criterion for 16 entries. One answer, the `D009` eviction, was not made: the free
  tier answered `429`, which counts as failing all five. Of the others, it missed the first step
  of `D006` and `D014` by the rubric's wording and one `D008` cause; it invented no object.
- `qwen3:4b` named the cause of every container and scheduling failure (`D001` to `D007`,
  `D009`, `D010`) and of both refusals (`D011`); it missed the `D008` causes that were in the log
  or in the reason (3 of 3 failed `cause`), and `D013` by the rubric's wording (below).
- `qwen3:0.6b` named a wrong cause for a missing ConfigMap key (`D004`: "insufficient CPU
  resources") and ignored the log of the crash loop (`D001`).
- Both invented an object for an incident whose request has no Pod (`D008`, `D011`, `D014`): a
  Pod or a Deployment in 3 answers of `qwen3:0.6b`, a Pod in 1 of `qwen3:4b`.
- The confidence criterion separated nothing: `qwen3:0.6b` answered `medium` 20 times, and every
  answer of either model was within its entry's `confidence_ok`.

## Per rule

Pass counts per criterion; entries per rule in brackets. A rule's "all" counts the entries that
pass the five criteria.

| Rule | Entries | 0.6b cause | 0.6b first step | 0.6b no invented | 0.6b all | 4b cause | 4b first step | 4b no invented | 4b all |
|---|---|---|---|---|---|---|---|---|---|
| `D001` crash loop | 1 | 0 | 1 | 1 | 0 | 1 | 1 | 1 | 1 |
| `D002` image cannot be pulled | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 |
| `D003` out of memory | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `D004` container cannot be created or started | 2 | 1 | 0 | 2 | 0 | 2 | 2 | 2 | 2 |
| `D005` pod cannot be scheduled | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 |
| `D006` probe failures | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `D007` volume cannot be attached or mounted | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `D008` job failed | 3 | 2 | 3 | 2 | 1 | 0 | 2 | 3 | 0 |
| `D009` pod evicted | 1 | 0 | 1 | 1 | 0 | 1 | 1 | 1 | 1 |
| `D010` pod sandbox cannot be created | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `D011` workload cannot create pods or jobs | 2 | 2 | 1 | 0 | 0 | 2 | 2 | 2 | 2 |
| `D012` volume claim cannot be provisioned | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `D013` autoscaler cannot get metrics | 1 | 0 | 1 | 1 | 0 | 0 | 1 | 1 | 0 |
| `D014` rollout does not progress | 1 | 1 | 0 | 1 | 0 | 1 | 1 | 0 | 0 |
| Total | 20 | 15 | 16 | 17 | 10 | 16 | 19 | 19 | 15 |

Every entry was valid and passed the confidence criterion for both models, so those columns are
left out. Fail counts are the entries minus the pass counts.

## Method

- **Cluster.** One throwaway kind cluster, node image `kindest/node:v1.36.4` pinned by the digest
  in `.github/workflows/e2e.yml` (Kubernetes v1.36.4), with its own kubeconfig; deleted at the end.
  `tests/bench/seeded-failures.yaml` applied once: the ten earlier objects and, added the same day,
  `neverpull`, `missing-key`, `no-entrypoint`, `no-volume`, `policy-job`, `indexed-job`,
  `evicted`, `no-handler`, `stalled` and the CronJob `report` in its own namespace.
- **kausa.** Built from commit `9afcf78` of the branch `feat/v0.5-coverage` (the coverage
  rules and this corpus) (`cargo build --locked
  --release`; it reports version `0.4.1`), run as `kausa run --dry-run` for 9 minutes, cluster-wide,
  with the default 60-second settle window. Every one of the 20 entries gave one `request` line,
  12 to 182 seconds after the objects were applied: the failed Jobs and the CronJob first (not
  waiting for the window), `D010` last (its events are aggregated first). The `PodFailurePolicy`
  request carried the note with the container and the Pod as `<redacted:name>` (`R003`) and the
  log tail of the failed Pod; the CronJob's carried `jobs.batch "<redacted:name>"` and the quota
  as `<redacted:name>`.
- **Models.** Ollama 0.9.2 on the same machine (an Apple M3 Max), at `127.0.0.1`:
  `qwen3:0.6b` (digest `7df6b6e09427a769808717c0a93cadc4ae99ed4eb8bf5ca557c90846becea435`, 751.63M
  parameters, Q4_K_M) and `qwen3:4b` (digest
  `359d7dd4bcdab3d86b87d73ac27966f4dbb9f5efdfcc75d34a8764a09474fae7`, 4.0B parameters, Q4_K_M).
  Each request was sent as logged, with only `model` replaced: temperature 0, `max_tokens` 800 and
  the answer's JSON schema in `response_format`. The 20 requests took 19 seconds with `qwen3:0.6b`
  and 65 seconds with `qwen3:4b`, one after the other.
- **Grading.** `python3 -I tests/bench/score.py --requests <dry-run log> --causes
  tests/bench/known-causes.json --endpoint http://127.0.0.1:<port>/v1 --model <model> --out
  <answers>`. The causes and their rubric were committed before the first model ran (commit
  `204de03` of the same branch, in its pull request's history), and not changed after. No model judged another. `--answers <answers>` grades the
  stored answers again with the same result.

## The rubric

For each answer (`tests/bench/score.py`):

- **valid**: the content is a JSON object with a non-empty `summary` and `cause`, one to five
  non-empty `steps` and a `confidence` of `low`, `medium` or `high`, as kausa reads it; an invalid
  answer fails the other four.
- **cause**: every group of the entry's `cause_all` has a case-insensitive substring in the summary
  and the cause.
- **first step**: one of `first_step_any` is in the first step.
- **confidence**: the confidence is in `confidence_ok`.
- **no invented object**: every placeholder-like name in the answer (`pod-2`, `deployment-1`, …) is
  in the request.

Keyword matching misses equivalent words and accepts wrong sentences that contain the right ones,
which is why the scope asks for a human pass.

## Human pass

Pending for the maintainer: no person has read the answers yet. An agent read the answers that
failed a criterion, which is no human pass; what it found is below, marked as its reading.

- **Rubric gaps (the agent's reading, not a grade change).** Three failures read as right answers
  in other words: `no-target` for both models ("cannot find the referenced deployment", "unable to
  find the deployment": the rubric lists "not found", "missing", "does not exist" but not "cannot
  find"), and `evicted` for `qwen3:0.6b` ("used more than the allocated capacity of 1Mi": the
  rubric wants "limit", "exceed" or "usage"). The rubric stays as committed; widening it after
  reading the answers would grade the models against their own words.
- **Partial answers.** `policy-job` for both models named the exit code 42 and the log's schema
  mismatch but not the pod failure policy that failed the Job at once, which the rubric asks for;
  `indexed-job` for `qwen3:4b` named the missing input file but not that one index failed.
- **Real misses.** `qwen3:0.6b`: `crasher` (a "failing to schedule" crash loop, the refused
  database connection in the log ignored), `missing-key` (CPU instead of the missing ConfigMap
  key), `no-entrypoint` (right cause, a first step about the runtime's health), `report` (a
  Deployment invented for a CronJob), `cannot-create` and `indexed-job` (a Pod invented),
  `stalled` (a first step about the rollout's settings, not its Pods). `qwen3:4b`: `failing-job`
  (the backoff limit as the cause, the "disk full" of the log ignored) and `stalled` (a Pod
  invented for an incident that sends none).

## Not detected

None: every seeded failure gave its request. The `D009` entry needs evictions in the last 10
minutes; on kind the kubelet evicted the `evicted` Deployment's Pods about once a minute for the
`emptyDir` `sizeLimit`, the first 40 seconds after creation.

## Do small models fail on known signatures?

Partly, and this is input for a possible future ADR on derived hints, not a decision. `qwen3:0.6b`
missed signatures a fixed rule could state from fields already sent: `CreateContainerConfigError`
with `configmap "<redacted:name>" not found`, and `ECONNREFUSED` in a log tail. Both models
invented a Pod or a workload for the incidents whose request has no Pod (`D008`, `D011`, `D014`),
and both treated a Job's failure reason (`PodFailurePolicy`, `BackoffLimitExceeded`) and its log
as competing causes rather than one chain. A hint per signature, or a prompt line that the request
of an event about a Job, a controller or a rollout has no Pod, are the candidates the scope names
(prompt wording, derived hints, or only a bigger model recommendation); one run of 20 entries does
not choose between them. Recommending `qwen3:4b` over `qwen3:0.6b` needs no ADR.
