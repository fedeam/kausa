# Explanation quality of six local models against known causes

How often six local models, `qwen3:0.6b`, `qwen3:4b`, `llama3.2:3b`, `phi4-mini:3.8b`,
`qwen2.5:7b` and `granite3.3:8b`, name the known cause of the 20 seeded failures of
`tests/bench/seeded-failures.yaml`, graded by the committed rubric (`tests/bench/known-causes.json`,
rubric 1) and by a draft rubric 2 that fixes the gaps of the [first measurement's human
pass](2026-11-02-explanation-quality.md#human-pass); measured once each on 2026-11-03, to decide
whether a derived `hints` request field needs an ADR (theme 4 of the v0.6 plan).

Once each, an order of magnitude: one dry-run, one answer per request and per model, on one
machine. A difference of one or two entries between models or rules is noise until it is
repeated. No hosted model was run; `gemini-3.5-flash-lite`'s 16 of 20 is from
[the 2026-11-02 measurement](2026-11-02-explanation-quality.md#result), on other requests.

## Result

Entries that pass every criterion (valid, cause, first step, confidence, no invented object), out
of 20, every one detected:

| Model | Parameters, quantization | Valid | Cause | First step | Confidence | No invented object | All, rubric 1 | All, rubric 2 (draft) | 20 requests |
|---|---|---|---|---|---|---|---|---|---|
| `qwen3:0.6b` | 751.63M, Q4_K_M | 20 | 15 | 16 | 20 | 17 | **10** | 12 | 24 s |
| `qwen3:4b` | 4.0B, Q4_K_M | 20 | 16 | 19 | 20 | 19 | **15** | 16 | 74 s |
| `llama3.2:3b` | 3.2B, Q4_K_M | 20 | 13 | 16 | 19 | 19 | **10** | 10 | 144 s |
| `phi4-mini:3.8b` | 3.8B, Q4_K_M | 20 | 18 | 18 | 19 | 20 | **16** | 16 | 163 s |
| `qwen2.5:7b` | 7.6B, Q4_K_M | 20 | 15 | 20 | 19 | 20 | **15** | 15 | 202 s |
| `granite3.3:8b` | 8.2B, Q4_K_M | 20 | 16 | 20 | 19 | 20 | **15** | 15 | 286 s |

The criteria columns are rubric 1; rubric 2 changes only `cause` (`qwen3:0.6b` 17, `qwen3:4b` 17,
the others unchanged).

- Every model answered valid JSON every time, with the request's own `response_format`.
- `qwen3:0.6b` and `qwen3:4b` gave the same counts as on 2026-11-02, per criterion and per rule
  (10 and 15), from a different build (`0.5.1` against `0.4.1`) and a different dry-run. Whether
  the answers are the same text is not known: the 2026-11-02 answers are not in the repository.
- No local model passed more entries than `phi4-mini:3.8b` (16), the count of the hosted
  `gemini-3.5-flash-lite` on 2026-11-02. The 7B and 8B models did not pass more entries than
  `qwen3:4b`; they never missed a first step and never invented an object.
- `policy-job` (`D008`) failed `cause` for all six models, each naming the log's schema mismatch
  and none the `FailJob` rule. `indexed-job` (`D008`) failed a criterion for all six: `cause` for
  four, the first step for `phi4-mini:3.8b`, and an invented `pod-1` for `qwen3:0.6b`, whose cause
  only repeated the event ("Job failed indexes") and passed `cause` through the word "fail".
- `stalled` (`D014`), whose request has no Pod, failed for five of six: a Pod invented by
  `qwen3:4b` and `llama3.2:3b`, `high` confidence from `llama3.2:3b`, `phi4-mini:3.8b` and
  `granite3.3:8b` (the entry accepts `low` or `medium`), a first step about the rollout's
  settings from `qwen3:0.6b` and `phi4-mini:3.8b`.
- The confidence criterion separated a little more than on 2026-11-02: four answers failed it,
  three of them the `D014` `high` above and one `low` from `qwen2.5:7b` on `no-target`.
- `llama3.2:3b` called three failures that have nothing to do with scheduling a scheduling
  problem: `crasher` ("unschedulable due to insufficient resources"), `unready` and `no-volume`
  ("affinity and tolerations"). Hypothesis, not tested: every Pod of a request carries
  `"unschedulable": null`, and the model read the key as evidence.

## Per rule

Entries that pass all five criteria, per rule and per model, rubric 1; entries per rule in
brackets.

| Rule | Entries | `qwen3:0.6b` | `qwen3:4b` | `llama3.2:3b` | `phi4-mini:3.8b` | `qwen2.5:7b` | `granite3.3:8b` |
|---|---|---|---|---|---|---|---|
| `D001` crash loop | 1 | 0 | 1 | 0 | 1 | 1 | 1 |
| `D002` image cannot be pulled | 2 | 2 | 2 | 1 | 2 | 2 | 1 |
| `D003` out of memory | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `D004` container cannot be created or started | 2 | 0 | 2 | 2 | 2 | 2 | 2 |
| `D005` pod cannot be scheduled | 2 | 2 | 2 | 1 | 2 | 1 | 2 |
| `D006` probe failures | 1 | 1 | 1 | 0 | 1 | 1 | 1 |
| `D007` volume cannot be attached or mounted | 1 | 1 | 1 | 0 | 1 | 1 | 1 |
| `D008` job failed | 3 | 1 | 0 | 0 | 1 | 1 | 1 |
| `D009` pod evicted | 1 | 0 | 1 | 0 | 1 | 1 | 1 |
| `D010` pod sandbox cannot be created | 1 | 1 | 1 | 1 | 0 | 0 | 1 |
| `D011` workload cannot create pods or jobs | 2 | 0 | 2 | 2 | 2 | 2 | 2 |
| `D012` volume claim cannot be provisioned | 1 | 1 | 1 | 1 | 1 | 1 | 1 |
| `D013` autoscaler cannot get metrics | 1 | 0 | 0 | 1 | 1 | 0 | 0 |
| `D014` rollout does not progress | 1 | 0 | 0 | 0 | 0 | 1 | 0 |
| Total | 20 | 10 | 15 | 10 | 16 | 15 | 15 |

With rubric 2, `D009` becomes 1 for `qwen3:0.6b` and `D013` becomes 1 for `qwen3:0.6b` and
`qwen3:4b`; no other cell changes.

The criteria each entry failed, rubric 1 (`C` cause, `F` first step, `K` confidence, `N` invented
object; `.` passed all; no answer was invalid):

| Entry | Rule | `qwen3:0.6b` | `qwen3:4b` | `llama3.2:3b` | `phi4-mini:3.8b` | `qwen2.5:7b` | `granite3.3:8b` |
|---|---|---|---|---|---|---|---|
| `crasher` | `D001` | C | . | C F | . | . | . |
| `badimage` | `D002` | . | . | . | . | . | C |
| `neverpull` | `D002` | . | . | C | . | . | . |
| `missing-key` | `D004` | C F | . | . | . | . | . |
| `no-entrypoint` | `D004` | F | . | . | . | . | . |
| `unplaceable` | `D005` | . | . | C | . | C | . |
| `unready` | `D006` | . | . | F | . | . | . |
| `no-volume` | `D007` | . | . | C F | . | . | . |
| `failing-job` | `D008` | . | C F | C | . | . | . |
| `policy-job` | `D008` | C | C | C | C | C | C |
| `indexed-job` | `D008` | N | C | C | F | C | C |
| `evicted` | `D009` | C | . | F | . | . | . |
| `no-handler` | `D010` | . | . | . | C | C | . |
| `cannot-create` | `D011` | N | . | . | . | . | . |
| `report` | `D011` | F N | . | . | . | . | . |
| `no-target` | `D013` | C | C | . | . | C K | C |
| `stalled` | `D014` | F | N | K N | F K | . | K |

`oom`, `toobig` and `no-class` passed for every model. Every
invented object was a Pod in a request with no Pod (`indexed-job`, `cannot-create`, `stalled`) or
a Deployment for the CronJob `report`.

## Method

- **Cluster.** One throwaway kind cluster (kind v0.33.0), node image `kindest/node:v1.36.4`
  pinned by the digest in `.github/workflows/e2e.yml` (Kubernetes v1.36.4), with its own
  kubeconfig; deleted at the end. `tests/bench/seeded-failures.yaml` applied once, one second
  after kausa started.
- **kausa.** Built from commit `1cee43e` of `main` (v0.5.1) with `cargo build --locked
  --release` (it reports version `0.5.1`), run as `kausa run --dry-run` for 9 minutes,
  cluster-wide, with the default 60-second settle window. Every one of the 20 entries gave one
  `request` line, 19 to 184 seconds after the objects were applied: the three failed Jobs and the
  CronJob first, `no-handler` (`D010`) last. The log has 20 `incident` and 20 `request` lines and
  no warning or error.
- **Models.** Ollama 0.9.2 on the same machine (an Apple M3 Max), at `127.0.0.1`. Ollama's
  documentation for that version lists `response_format` among the supported fields of its
  OpenAI-compatible endpoint and describes structured outputs; whether it constrains decoding to
  the schema or only passes it on is not verified (a hypothesis either way). Models, as
  `/api/tags` reported them:

  | Model | Digest | Family |
  |---|---|---|
  | `qwen3:0.6b` | `7df6b6e09427a769808717c0a93cadc4ae99ed4eb8bf5ca557c90846becea435` | qwen3 |
  | `qwen3:4b` | `359d7dd4bcdab3d86b87d73ac27966f4dbb9f5efdfcc75d34a8764a09474fae7` | qwen3 |
  | `llama3.2:3b` | `a80c4f17acd55265feec403c7aef86be0c25983ab279d83f3bcd3abbcb5b8b72` | llama |
  | `phi4-mini:3.8b` | `78fad5d182a7c33065e153a5f8ba210754207ba9d91973f57dffa7f487363753` | phi3 |
  | `qwen2.5:7b` | `845dbda0ea48ed749caafd9e6037047aa19acfcfd82e704d7ca97d631a0b697e` | qwen2 |
  | `granite3.3:8b` | `fd429f23b90980ed1bef53b990894e7b0199331f6ae90c5650240a7d5b70f1f7` | granite |

  `gemma3:4b` was the fourth candidate; Ollama 0.9.2 could not pull it (`412`: the model needs a
  newer Ollama), and `granite3.3:8b` took its place. Each request was sent as logged, with only
  `model` replaced: temperature 0, `max_tokens` 800 and the answer's JSON schema in
  `response_format`. The models ran one after the other; the times in the result are wall-clock
  for the 20 requests, including the first load of each model.
- **Grading.** `python3 -I tests/bench/score.py --requests <dry-run log> --causes
  tests/bench/known-causes.json --endpoint http://127.0.0.1:<port>/v1 --model <model> --out
  <answers>` (Python 3.15.0), then `--answers <answers>` with the same causes file (the same
  output, byte for byte, for every model) and with the rubric 2 draft. Neither `score.py` nor
  `known-causes.json` was changed. No model judged another.

## Rubric 2 (draft)

Rubric 2 was written on 2026-11-03 before any model of that day ran, from the two gaps the
2026-11-02 human pass lists and nothing else, and the stored answers were graded with it
afterwards. Rubric 1 stays as committed, so its results stay reproducible.

- **Versioning.** A new file next to the old one (`tests/bench/known-causes-v2.json`, if it is
  committed), with the same entries and schema, `"version": 1` (the file format `score.py`
  reads, `tests/bench/score.py:67`) and a new top-level `"rubric": 2` with a note of the changes,
  which `score.py` ignores. A note grades with the file it names; `known-causes.json` stays rubric 1.
- **"Cannot find".** Every cause group that accepts `not found` also accepts `cannot find`,
  `can't find`, `could not find`, `couldn't find`, `unable to find`, `failed to find`,
  `cannot be found` and `could not be found`: the `no-target` gap, applied to all eight such
  groups (`badimage`, `neverpull`, `missing-key`, `no-entrypoint`, `no-volume`, `no-handler`,
  `no-class`, `no-target`) so that the equivalence is the same for every entry.
- **"More than".** `evicted`'s second cause group also accepts `more than`, `capacity`,
  `too much`, `larger than`, `bigger than` and `beyond`: the `evicted` gap.

Rubric 2 turns three failures into passes, the three the 2026-11-02 human pass named:
`no-target` for `qwen3:0.6b` and `qwen3:4b`, and `evicted` for `qwen3:0.6b`. It changes nothing
for the four new models.

## Human pass

Pending for the maintainer: no person has read the answers yet. An agent read every answer that
failed a criterion, which is no human pass; what it found is below, marked as its reading.

- **More rubric gaps (the agent's reading, not applied).** Four failures read as right answers in
  other words that rubric 2 does not list: `no-target` for `granite3.3:8b` ("the deployment not
  being found"), `neverpull` for `llama3.2:3b` ("the image policy is set to Never, which means it
  will never attempt to pull the image"), `no-handler` for `phi4-mini:3.8b` ("Unconfigured OCI
  Runtime Handler") and for `qwen2.5:7b` ("not installed or configured"). They are candidates for
  a rubric 3 written before the next run; adding them now would grade these models against their
  own words.
- **Partial answers.** `policy-job` (all six) and `indexed-job` (four on `cause`, one on the
  first step) named the error of the log but not how the Job failed (the `FailJob` rule, the
  failed index). `badimage` for `granite3.3:8b` named the pull failure and the image with its tag,
  and its first step says to correct the tag, but its cause does not say that the tag does not
  exist. `unplaceable` for `qwen2.5:7b` named the node affinity and selector but not that no node
  matches them.
- **Real misses.** `qwen3:0.6b`: as on 2026-11-02 (`crasher`, `missing-key`, `no-entrypoint`'s
  first step, `report`, `cannot-create`, `indexed-job`, `stalled`). `qwen3:4b`: `failing-job`
  (the backoff limit as the cause, the log's "disk full" ignored) and `stalled` (a Pod invented).
  `llama3.2:3b`: `crasher`, `unready` and `no-volume` as scheduling failures, `failing-job` as the
  backoff limit, `unplaceable` as "insufficient node affinity", `evicted`'s first step about the
  Deployment's resources, and `stalled` (a Pod invented, `high`). `phi4-mini:3.8b`: `stalled`
  (`high`, a first step about the replica count). `qwen2.5:7b`: `no-target` ("unable to retrieve
  the scale information", `low`). `granite3.3:8b`: `stalled` (`high`).
- **A corpus artifact.** In the `evicted` request, each Pod's `reason` is `deployment-1`, not
  `Evicted`: the Deployment is named `evicted`, and `R002` replaces a known name in any ASCII case
  ([Detection and redaction rules](../reference/rules.md), `R002`). The detection's name ("pod
  evicted") and the status message still say it; no model failed the entry's cause for it, but
  the seeded name hides the reason a real eviction would show.
- **A redaction gap, fixed.** The `no-volume` request carried the name of the missing Secret in
  clear: a volume named like a secret key made `R010` take the kind for its value, so `R003` no
  longer saw `secret "<name>"`. Fixed in v0.5.2
  ([#76](https://github.com/fedeam/kausa/pull/76)); only the object's name was exposed, never a
  value.

## Not detected

None: every seeded failure gave its request.

## Hints

Every known signature that a model failed, with what the request already carried and whether a
hint derived from fields kausa already sends (a fixed sentence chosen by the detection, the event
reasons, the waiting reasons and the presence or absence of Pods, never new cluster text) would
plausibly have fixed it. "Plausibly" is the agent's reading of the answers, not a measurement:
no hint was tried.

| Signature (detection, cause) | Failed | What the request already says | A derived hint would plausibly fix it? |
|---|---|---|---|
| `D008`, a `podFailurePolicy` `FailJob` rule ends the Job at the first failure (`policy-job`) | 6 of 6, `cause` | Event reason `PodFailurePolicy`, note "failed with exit code 42 matching FailJob rule at index 0", and the log | **Yes, likely.** Every model read the log and skipped the event. A sentence from the reason ("the Job's pod failure policy failed the Job at this exit code, with retries left") states the chain the rubric asks for. The operator's cause (the schema mismatch) was already named by all six |
| `D008`, one index of an indexed Job fails (`indexed-job`) | 6 of 6 (4 `cause`, 1 first step, 1 invented Pod) | Event reason `FailedIndexes` ("Job has failed indexes") and the log ("shard 1: input file missing") | **Yes, likely** for `cause` ("an indexed Job: some indexes failed and the others did not"). The missing input file was already named |
| `D008`, the log's error is why, the backoff limit is how (`failing-job`) | 2 of 6 (`qwen3:4b`, `llama3.2:3b`) | Event reason `BackoffLimitExceeded` and the log ("report failed: disk full") | **Plausibly.** A sentence that `BackoffLimitExceeded` is how the Job ended and the log tail is why. The three larger models did it without one |
| `D008`, `D011`, `D014`: the request has no Pod | 5 answers invented a Pod or a workload (`qwen3:0.6b` 3, `qwen3:4b` 1, `llama3.2:3b` 1 of `stalled`) | `pods` is empty; [Data sent to the provider](../reference/data-sent-to-the-provider.md) documents it per rule | **Yes, likely**, and it needs no field: one prompt line ("when `pods` is empty, kausa sent no Pod; do not name one") is a wording change of the system message |
| `D014`, the rollout passed its deadline; only counts are sent (`stalled`) | 5 of 6 (`high` 3, a Pod 2, first step 2) | `rollout` with seven integers, no Pod, no event, no log | **Plausibly**, for the invented Pod and the `high` confidence: "only the rollout's counts are sent; the Pods' state is not in the evidence". The first step is the rubric's wording ("pod", "event", "describe", "rollout status", "replicaset", "schedul") |
| `D001`, the log shows a refused connection to the database (`crasher`) | 2 of 6 (`qwen3:0.6b`, `llama3.2:3b`) | `log_tail` "connect ECONNREFUSED <redacted:ip>:5432", exit code 1 | **Plausibly**: both answers ignored the log; a hint would repeat a line of it. It would turn kausa into a log classifier for one pattern, which the other four models did not need |
| `D004`, a ConfigMap key the container needs does not exist (`missing-key`) | 1 of 6 (`qwen3:0.6b`) | Waiting reason `CreateContainerConfigError`, message `configmap "<redacted:name>" not found` | **Plausibly**, from the waiting reason and its message; one model of six |
| `D007`, a Secret volume does not exist (`no-volume`) | 1 of 6 (`llama3.2:3b`) | Event reason `FailedMount` and its note | **Unclear**: the same model called two other failures scheduling problems; the miss looks like the model, not the signature |
| `D013`, the scale target does not exist (`no-target`) | 4 of 6 by rubric 1, 2 by rubric 2; 1 real (`qwen2.5:7b`) | Event reason `FailedGetScale`, note `deployments.apps "<redacted:name>" not found` | **Plausibly** for `qwen2.5:7b`; the others are the rubric's wording |
| `D005`, `D006`, `D009`, `D010`, `D002` (`unplaceable`, `unready`, `evicted`, `no-handler`, `badimage`, `neverpull`) | 1 or 2 each | The scheduler's message, the probe event, the eviction message, the sandbox events, the waiting message | **No**: the request already states the cause in a sentence; the failures are the rubric's wording, a first step, or one model |

**The measurement's reading, not a decision.** Small models do fail on known signatures, and the
failures cluster in two places that a field kausa already sends identifies exactly: a Job's
failure reason against its log (`D008`, 14 of 18 answers failed a criterion, `policy-job` for
every model, 7B and 8B included), and incidents whose request has no Pod (`D008`, `D011`, `D014`:
every invented object, and three of the four confidence failures). Elsewhere a hint would repeat
a sentence the request already holds, or fix one model on one entry.

An ADR for a derived `hints` request field (0024, with `PROMPT_VERSION` 3) is justified to write
as `proposed`, scoped to those two clusters, with prompt wording alone (a system message line
about requests with no Pod and about a Job's reason against its log, `PROMPT_VERSION` 3, no new
field) as its first alternative: that alternative changes nothing that leaves the cluster and
covers part of the same failures. The decision should rest on a before-and-after run of this
corpus graded with a rubric fixed before the run (rubric 2, or a rubric 3), with the same six
models; one run of 20 entries does not choose between a hint field and a prompt line. Recommending a
bigger local model needs no ADR: here `phi4-mini:3.8b` matched the hosted model's 16, and no 7B
or 8B model passed more than `qwen3:4b`.

The decision that follows from this reading is [0024](../adr/0024-system-message-by-detection.md).
