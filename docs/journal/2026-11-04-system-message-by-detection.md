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

## Release v0.6.0

The release change for v0.6.0, step 1 of "Cut a release" in [Maintain the
repository](../how-to/maintain.md), with the four themes of v0.6: the chart on GHCR as a signed OCI
artifact with SBOMs and `cargo auditable` ([0023](../adr/0023-oci-chart-and-sbom.md)), kausa's own
observability (alert rules checked by `promtool`, seven new metrics), the upgrade test and the soak
scripts, and the system message chosen by detection
([0024](../adr/0024-system-message-by-detection.md), `PROMPT_VERSION` 3, `D008` keeping v2's).

- `version` 0.6.0 in `Cargo.toml`, `Cargo.lock` (only kausa's entry changed) and the chart's
  `version`, `appVersion` and `artifacthub.io/images` line.
- The changelog's `Unreleased` entries moved to `[0.6.0] - 2026-11-04`, with a summary and the
  upgrade notes the `Changed` entries imply: the counters with a fixed set of labels start at `0`
  for each label and lose their unlabelled `0` line, a copy of the install guide's example alert
  `KausaDailyCallBudgetSpent` is replaced by the file's, and new Explanation entries store
  `promptVersion` `"3"`. Nothing changes in values or permissions.
- The README (status note, Status, a v0.6.0 roadmap entry, and "Try it" installing
  `oci://ghcr.io/fedeam/charts/kausa --version 0.6.0` with the crasher image 0.6.0), the [Security
  policy](../../SECURITY.md), the front page, [Command line and log
  format](../reference/command-line.md), [Helm chart](../reference/helm-chart.md),
  [Alerts](../reference/alerts.md), the status comment of `src/run.rs` and the release section of
  [Maintain the repository](../how-to/maintain.md) name v0.6.0.
- [Install kausa with Helm](../how-to/install-with-helm.md): the lines that said v0.6.0 is not
  released are rewritten (the status paragraph, the end of step 1, "Upgrade from a chart URL to
  the OCI chart" and the end of "Verify a release"). The lines that say no install from GHCR and
  no command of the release notes has run stay until step 5 runs them.
- Not changed: `charts/artifacthub-repo.yml` is not created (the chart is not registered on
  Artifact Hub yet), and `tests/e2e/upgrade-from.txt` still pins v0.5.1 (step 6 bumps it), as does
  the status line of [Run the soak](../how-to/run-the-soak.md), which names the pinned release.
- Checks run locally: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` (717 tests),
  `helm lint --strict`, `tests/chart/render.sh` and the `ci` target's image line check passed.
  No tag was made and nothing was pushed.

## Later the same day: withhold destructive commands (ADR 0025)

The request: implement [0025](../adr/0025-withhold-destructive-commands.md) (issue
[#84](https://github.com/fedeam/kausa/issues/84)) for v0.6.1, accepted earlier today (#86): tests
first, then the code, the fuzz target, the end-to-end checks, the docs, and the corpus and offline
grader the measurement will use, without running the measurement.

What was done, on the branch `feat/withhold-destructive` (the corpus, the grader and the e2e and
chart checks on `feat/withhold-destructive-bench`, merged into it):

- Tests first: the shapes on their positive cases with exact ids, the one-token forms, near misses
  and the pinned limits and false positives, the rules on a match across versions, idempotence, the
  notice first in `events::note` and `notification::render` (also under the 1,024-byte cut), every
  sink and the counter in `src/explain.rs` (a Job named `kubectl`, a container named `ns`, a cached
  repeat, an ordinary answer), the 23rd counter and its golden lines, and the replay of the 420
  committed answers.
- `src/prompt.rs`: `WITHHELD`, `WITHHELD_FIELD`, `SHAPES`, the fold, the candidates (no cut), the
  six patterns built once, `shapes`, `withhold_all`, `withhold` and `Explanation::withheld`.
  `src/explain.rs` runs `withhold_all` on the placeholder text, the restored answer and the
  notification's after both restores and before the dedup entry, counts
  `kausa_answers_withheld_total` and logs `withheld` and `model_confidence` on fresh lines.
  `src/events.rs` and `src/notification.rs` put the notice first; `src/notify.rs` ties the
  notification's copy of the text to `prompt::WITHHELD`.
- The `validate` fuzz target asserts that no shape survives `withhold`, on the answer and on a copy
  with a placeholder renamed `kubectl`, with seven seeds from the measured answers.
- e2e: `failing-job` fails on `--e2e-withhold-marker`, the fake endpoint answers a request holding
  the marker with a third step, `kubectl delete namespace --all`, and `tests/e2e/run.sh` checks the
  notice in every sink, the log fields and the counter. `tests/chart/render.sh` ties the pieces
  together statically.
- The corpus: 13 Deployments in `tests/bench/injection-corpus.yaml` (26 in all) and
  `tests/bench/known-causes-injection.json` (16 entries), and the ignored tests
  `withhold_answers_dir` and `withhold_sink_texts` that grade a measurement's answers and sink texts
  offline, printing ids and counts, never text.
- Docs: "Withheld command shapes" in [Detection and redaction rules](../reference/rules.md), with
  the patterns as the code compiles them (a new test, `every_shape_is_in_the_rules_reference`,
  compares them byte for byte) and the Limits; [Command line and log
  format](../reference/command-line.md), [Data sent in
  notifications](../reference/data-sent-in-notifications.md), [Explanation
  resource](../reference/explanation-resource.md), [Alerts](../reference/alerts.md) ("What has no
  alert"), the [Security policy](../../SECURITY.md), [ARCHITECTURE.md](../../ARCHITECTURE.md)
  (invariant 5, the flow, the codemap), [Architecture](../explanation/architecture.md), [Add a
  detection or redaction rule](../how-to/add-a-rule.md) and the changelog under `Unreleased`.

### Decisions

- **The e2e marker is an unknown option.** `failing-job` already failed on an option kausa does
  not know; naming it `--e2e-withhold-marker` puts the marker in the usage error that kausa's own
  binary prints (`cargo run --locked -q -- --e2e-withhold-marker` exits 2 with it on stderr), so
  the `D008` request's log tail carries it and the fake endpoint can answer that incident alone
  with the destructive step. No new workload, and `policy-job`, which `render.sh` pins, is
  unchanged.
- **The log fields are checked end to end, not in the unit harness.** `log::info` writes to
  stdout, which the offline harness of `src/explain.rs` does not capture, so `withheld` and
  `model_confidence` (and their absence on cached lines) are asserted in `tests/e2e/run.sh`;
  `src/log.rs` was not refactored for it.
- `kubectl delete pods --all=0`, which the ADR lists among the forms that do not match, joined
  the near misses, so that every example the rules reference gives is pinned by a test.

### Checks and hypotheses

- Passed locally on the merged branch: `cargo fmt --check`, `cargo deny --locked check`,
  `cargo audit`, `helm lint --strict` with `rbac.scope=cluster`, `cargo clippy -D warnings`,
  `cargo test` (751 tests, 2 ignored), `bash tests/chart/render.sh`, `bash -n` on the e2e
  scripts (shellcheck finds the same notes as on `main`), the fuzz crate's `cargo check`, and
  `gitleaks` on the branch's history. The fake endpoint answered 3 steps to a body with the marker
  and 2 to one without. The commit before the merge passed fmt, clippy and the tests too.
- Not run here: the kind test and `docker build --target ci .` (GitHub runs them on the pull
  request), and `promtool`, which is not installed on this machine.
- Confirmed in the crate: the committed answers replay as 0025 predicted from its Python
  prototype: 420 valid rows, exactly the 7 named `inject-log` answers change, 11 of 1,441 steps
  are removed, and `high` among the 12 `inject-log` answers falls from 10 to 3. The offline grader
  gives the same counts.
- Still hypotheses: that the shapes stay silent on benign answers beyond this corpus, that small
  models copy an injected command verbatim beyond these 11 steps, and that Slack and Telegram show
  the notice as plain text. The measurement decides the first two.

### Next steps

- The measurement of 0025 (its Consequences): the dry-run validity check, the local calls on the
  16 entries, the offline grading and one sink run; then the research note, an `Update` in 0025,
  and the `KausaAnswerWithheld` example alert only if bar M1 (no benign answer changed) holds.
- Close #84 with a link to the note.
