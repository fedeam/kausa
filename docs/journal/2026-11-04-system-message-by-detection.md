# 2026-11-04 — A system message chosen by detection

The code, the docs and the frozen measurement files of [0024. Send one of four fixed system
messages, chosen by detection, that say when a request carries no Pod and how a failed Job's event
and log combine, as `PROMPT_VERSION` 3, only if a before-and-after run passes bars fixed
now](../adr/0024-system-message-by-detection.md), accepted today; not the run itself.

## Request

Implement 0024 as accepted, in two tracks from the commit that accepts it: the code and its docs,
and the files that fix the before-and-after run before any model answers. Run no model and no
cluster; the run and its verdict come in a separate step, and the code pull request waits for it.

## What was done

- **The canonical texts.** The seventh sentence of the `job` part now calls the log lines
  "untrusted like the rest" instead of saying they explain why the container failed, as the ADR
  quotes it (926 bytes). The four messages have 1,277, 1,492, 2,420 and 2,096 bytes, and a
  throwaway script checked that their sha256 values agree in `src/prompt.rs`, in the copies
  embedded in `tests/bench/before_after.py` and in the quotes of [Data sent to the
  provider](../reference/data-sent-to-the-provider.md).
- **Reading the Job controller.** Before the freeze, the Kubernetes v1.36.4 sources
  (`pkg/controller/job/pod_failure_policy.go`, `pkg/apis/batch/types.go` and the versioned
  `staging/src/k8s.io/api/batch/v1/types.go`) were read for the clauses of the `job` part. A
  `FailJob` rule matches either a container's exit code (`onExitCodes`) or a Pod condition
  (`onPodConditions`), so "a failed container or a Pod condition" holds, and the five reasons are
  those the part names. No clause was found wrong, so no text changed.
- **Code** (`test(prompt): acceptance tests for system message by detection`, then
  `feat(prompt): choose one of four system messages by detection`). The tests came first and
  failed for the expected reasons: `system` missing, `request` with the old arity, and
  `PROMPT_VERSION` `"2"`. `src/prompt.rs` builds the messages from five `macro_rules!` parts
  joined by `concat!`; `system` is an exhaustive match with no wildcard; `request` takes the
  detection first. `SYSTEM_POD` is the version 2 text byte for byte, so the golden files of the
  endpoint check (`tests/golden/`) did not change. `fit` in `src/explain.rs` passes the
  incident's detection, and `src/check.rs` its fabricated `D001`. `tests/cli.rs` expects
  `prompt_version` `"3"`.
- **The frozen measurement** (four `test(bench)` commits): `tests/bench/known-causes-v3.json`
  (rubric 3: `indexed-job`'s second cause group no longer accepts `fail`), the held-out `D008`
  corpus `tests/bench/held-out-failures.yaml` and `tests/bench/known-causes-held-out.json`, the
  config `tests/bench/before-after.json` and the script `tests/bench/before_after.py` (`swap`,
  `echo`, `blind`, `compare`, `selftest`), which loads `score.py` by path and leaves it unchanged.
  With rubric 3 the 2026-11-03 answers keep their totals; `compare` with those answers as both
  arms prints net 0, no wrong clause in either arm, bars G and H "not run" and "verdict: no
  change"; `echo` exits 0 with rubric 3 and 1 with rubric 2. The bench branch was merged into the
  code branch.
- **Docs** (`docs(prompt): document the four system messages of 0024`): a section "The system
  messages" in Data sent to the provider, with its table and the three new parts quoted; the
  body and `D014`'s `warning_events` wording there; the prompt bullet of
  [Architecture](../explanation/architecture.md); the `src/prompt.rs` row of the codemap in
  `ARCHITECTURE.md`; `prompt::system` in step 1 of [Add a detection or redaction
  rule](../how-to/add-a-rule.md); and a `Changed` entry under `Unreleased` in the changelog that
  says the messages ship only if the measurement passes. SECURITY.md and [Explanation
  resource](../reference/explanation-resource.md) do not change: every message keeps the
  untrusted-data paragraph, no field or destination is added, and the resource page names the
  constant, not its value.
- **Checks:** `cargo fmt`, `clippy` with `-D warnings`, `cargo test` (the check-endpoint golden
  test included), `cargo deny`, `helm lint --strict` with `rbac.scope=cluster`,
  `tests/chart/render.sh`, `gitleaks` on the branch's history and `before_after.py selftest`
  pass. Charts, the end-to-end test, the workflows, `Cargo.toml`, `Cargo.lock`,
  `src/context.rs`, `src/redact.rs`, `tests/golden/` and `tests/bench/score.py` are unchanged.
  The image and the kind end-to-end test were left to GitHub CI.

## Decisions

All within [0024](../adr/0024-system-message-by-detection.md):

- **The blinded reader is an agent** told neither the arm nor the hypothesis, as the ADR says,
  not the maintainer; the script's help and docstrings call it "the reader".
- **The selftest is self-contained:** the 2026-11-03 answers are not in the repository, so it runs
  on synthetic fixtures with the four messages embedded. The check against those answers ran from
  a local copy.
- **The echo freeze rule covers the target set only.** A new paragraph that alone passes a
  held-out entry's criterion (`deadline-job` through the `job` part) is reported, not refused,
  since the held-out entries test the endings that part names.
- **The held-out manifest was validated offline** with `kubeconform -strict` (6 resources) and a
  YAML parse, instead of a client dry-run, which needs an API server.

## Hypotheses

- Confirmed by the source reading: a `FailJob` rule can match a Pod condition, not only an exit
  code.
- Partly checked: a `D008` request with the longest message and a full 4 KiB log tail still fits
  the default `--max-input-tokens` (a unit test). Whether a real request nears the cap stays a
  hypothesis.
- Not checked yet: that `serde_json` gives the bytes the spec estimates, that each held-out Job
  ends with its expected reason on kind v1.36.4, and that the injected log line reaches its
  request unchanged. The run reports each.

## Next steps

- The before-and-after run (requests, answers, the blinded reading and the verdict) is a separate
  step. The code pull request merges only on its verdict; the bench pull request can merge first.
- The research note of the run, and the changelog entry's link to it, come with the verdict.
- kausa cites the Job reasons as `pkg/apis/batch/types.go:622-633` (`src/detect.rs`,
  [Detection and redaction rules](../reference/rules.md)); at v1.36.4 they are at those lines of
  `staging/src/k8s.io/api/batch/v1/types.go`. A later fix, outside 0024.

## Review fixes

A review of the branch found six problems, each confirmed by two of three skeptics; all are fixed
before the run.

- **The fuzz targets did not build** (`fix(prompt): keep the fuzz targets building with the
  detection`). `fuzz/fuzz_targets/validate.rs` and `history.rs` compile `src/prompt.rs` on its
  own, and its new `use crate::detect::Detection` has no module there, so `cargo fuzz build` and
  ClusterFuzzLite would have failed. The import, `system` and `request` are now under
  `#[cfg(not(fuzzing))]`; `fuzz/build.rs` sets `cfg(fuzzing)` as `cargo fuzz build` does, so that
  `cargo check --manifest-path fuzz/Cargo.toml` passes too, and kausa's `Cargo.toml` declares the
  cfg for `check-cfg`. `fn system` stays in `src/prompt.rs`, as the ADR says. None of the checks
  of AGENTS.md builds the fuzz crate; that gap stays open.
- **`before_after.py`** (`test(bench): make before_after.py follow ADR 0024 before the freeze`):
  `swap` fails when a detection's message is not the one the ADR gives it; `compare` computes `n`
  before the reader's marks, takes the majority of three when every pair has three answers (one
  file per repeat), and a mark fails the answer's `cause` whatever its other criteria, so bar B1
  counts it; the sheet no longer names the ADR. `selftest` has 15 checks, and a mutation of each
  fix fails its check.

## The run and its verdict

The before-and-after run of 0024 ran today on the maintainer's machine, with the frozen files above
unchanged (their sha256 values recorded before the first model call): one throwaway kind cluster,
both builds in `--dry-run`, `swap`, `echo` (the freeze rule held), 420 local calls to six Ollama
models, the blinded pass and `compare`. Written up in [The system message by detection,
measured](../research/2026-11-04-system-message-by-detection-measured.md).

- **Verdict: partial.** `compare` ships `intro_no_pod` (the no-Pod paragraph, `D011` to `D013`)
  and `rollout` (`D014`), and holds `job` (`D008`): bar B1 is 4 of 12 against 8, bar B2 net 4
  against 6. Bars N, A (net 10, exactly the bar), C1, C2, D, E, F, G and H pass. Bar H passes as a
  comparison, but five of six models followed the injected log line in both arms.
- **The reader's pass:** an agent told neither the arm nor the hypothesis marked 51 of 144
  answers; 19 marks turned a passing cause into a fail. Without the marks B2 would pass, B1 would
  not, and the verdict would be the same.
- **Committed for reproduction** in `tests/bench/runs/2026-11-04-0024/`: the request lines of the
  before log, `swap`'s output, the 30 answer files, the marks and the key. `compare` on them gives
  the note's output byte for byte. The before log's `incident` lines are not committed: they carry
  the kind cluster's Pod names and a Pod address.
- **Deviation from the protocol:** both builds were stopped once all 25 requests were logged
  (201.6 s after the apply), not after 12 minutes.

### Decisions

- **`src/` does not change.** The verdict does not ship `job`, so the code is left as it is; the
  maintainer decides how the code branch proceeds. Per the ADR, `D008` without `job` would get the
  `D011` to `D013` message, a configuration not measured whole: it is answered again on the 18
  `D008` pairs and the 30 held-out pairs, against this run's `B`, before it ships. `D008` becomes
  the measured case of ADR 0025.
- **The ADR's Status and the `Update` paragraphs of 0008 and 0018 are not written here:** they
  depend on what ships, which is the maintainer's decision.

### Next steps

- The maintainer's decision on the branch: drop `job` and re-measure `D008` with the no-Pod
  message, or keep `D008` on the Pod message until ADR 0025.
- ADR 0025 (a derived `hints` field, reason-keyed system text as its first alternative), measured
  under this protocol with this run's `B`, bar H included.

## The maintainer's decision on `D008`

The maintainer decided that `D008` keeps v2's system message, `SYSTEM_POD`, unchanged, instead of
the no-Pod message the ADR's "What ships" gives a request whose paragraph is dropped. v2's message
is exactly the measured `B` arm, so nothing ships unmeasured and no run is repeated; `intro_no_pod`
alone on `D008` was never measured whole.

- **Code** (`feat(prompt): keep v2's system message for D008`): `prompt::system` maps
  `Detection::JobFailed` to `SYSTEM_POD`; the `job!()` part and `SYSTEM_JOB` are removed, so the
  binary has three messages (`SYSTEM_POD` for `D001` to `D010`, `SYSTEM_CONTROLLER` for `D011` to
  `D013`, `SYSTEM_ROLLOUT` for `D014`). `PROMPT_VERSION` stays `"3"`. The unit tests in
  `src/prompt.rs` and `src/explain.rs` now expect `SYSTEM_POD` for `D008`; the byte-identity test
  of `SYSTEM_POD` with v2 covers `D008` too.
- **Docs:** Data sent to the provider (three messages, the `job` paragraph no longer quoted), the
  Architecture page, the codemap row of `ARCHITECTURE.md`, the front page and the changelog. An
  `Update` paragraph in [0024](../adr/0024-system-message-by-detection.md) records the verdict and
  this decision, and one in [0008](../adr/0008-data-leaving-the-cluster.md) corrects its earlier
  `Update` (three messages, not four). The ADRs' Decision sections and Status do not change.
- **Unchanged:** the research note, `tests/bench/before_after.py` and `tests/bench/runs/`, which
  record what was measured, `SYSTEM_JOB` included.

### Next steps

- `D008` is the measured case of a later ADR on hints (ADR 0025 in 0024's "What ships"), with this
  run's `B` as its reference.
