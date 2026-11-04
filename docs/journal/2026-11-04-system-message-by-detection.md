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
