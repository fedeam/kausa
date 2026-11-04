# Add a detection or redaction rule

The files to change, the tests to write and the docs to update for a new rule, or a new case of
an existing one. What a rule needs to be accepted (an id, evidence, fixtures, expected output) is
in [Contributing to kausa](../../CONTRIBUTING.md#propose-a-rule); the rules themselves are in
[Detection and redaction rules](../reference/rules.md).

Status as of 2026-11-02: fourteen detection rules (`D001` to `D014`) and fourteen redaction rules
(`R001` to `R014`), after the new cases of `D002`, `D004`, `D008`, `D011` and `R003` of
2026-11-02; the paths below are the ones in the code today.

## Before you start

- A **Rule request** issue on GitHub that the maintainer has agreed to, with the evidence: for a
  detection, the Kubernetes documentation or source that shows how the failure appears in Pod
  status or events; for a redaction, the issuer's published format of the value.
- A development environment ([Set up a development environment and run the checks](develop.md)).
  The rules are plain functions and regular expressions: their tests need no cluster.
- Invented fixtures only: `example.com`, `192.0.2.0/24`, `2001:db8::/32`, the AWS account
  `123456789012`, token strings built to match a format. Never real output, even sanitized.

## Add a detection rule

1. **Name it** in `src/detect.rs`: a new variant of `Detection`, with a doc comment that starts
   with its id (`/// \`D009\`: …`), its arms in `Detection::id` and `Detection::name`, and its
   place in `ALL`, the list of every detection in the tests module. Then decide, for the new
   variant:
   - `Detection::settles`: whether it must persist through the settle window (everything but a
     final failure such as a failed Job);
   - `Detection::is_state`: whether the signal is a state read from the Pod at every check
     (container states, a Pod condition) rather than an event;
   - `reads_logs` in `src/explain.rs`: whether the explanation needs the failed container's log
     tail. Reading logs needs `pods/log`, so a new detection that reads them is a decision about
     what kausa reads ([0009](../adr/0009-least-privilege-rbac.md)).
   - `prompt::system` in `src/prompt.rs`: which fixed system message the new detection's request
     carries. A new paragraph, or a message for a new kind of incident, needs a measurement like
     [0024](../adr/0024-system-message-by-detection.md)'s.

   `is_state`, `reads_logs` and `prompt::system` are exhaustive matches with one arm per variant,
   so the crate does not compile until you add the new variant to all three: give each answer on
   purpose.
2. **Detect it**, in the function for its kind of signal:
   - a container state (waiting or terminated reason, exit code): `detect_container` in
     `src/detect.rs`;
   - a Pod condition: a function like `detect_unschedulable`, called from `collect_signals` in
     `src/incident.rs`;
   - a Warning event (its reason, and the kind of object it is about): `detect_event` in
     `src/detect.rs`. An event about an object other than a Pod is keyed in `collect_signals` on
     that object's top-level owner, which `run.rs` resolves only through the hops of
     `src/owner.rs`; a new hop is a new permission;
   - a Deployment's condition: a pure function over the `DeploymentRecord` of `src/record.rs`, like
     `detect_rollout`, turned into signals by a function like `rollout_signals` in
     `src/incident.rs`. A rule on another kind of object needs a watch on it, which is a new
     permission and an ADR ([0018](../adr/0018-stuck-rollouts.md)).

   If the rule needs a field kausa does not record yet, it goes into the small records of
   `src/record.rs` first. A field that would reach the model's request is a decision about data
   leaving the cluster: it needs an ADR and an update to [Data sent to the
   provider](../reference/data-sent-to-the-provider.md) in the same change.
3. **Test it** in the `tests` module of `src/detect.rs`: the status or event that triggers the
   rule, and the closest ones that must not (a single restart during a rollout, a `Completed`
   container, an event about another kind). If the rule changes how incidents are grouped or when
   they settle, add a case to the tests of `src/incident.rs` as well.

   **A rule that yields.** When the new rule can describe the same failure as another rule on the
   same key, decide which one is explained. `D014` shows the pattern: `Tracker::tick` decides
   every other key first and the yielding keys after them, so the order of the keys does not
   matter; a yielding key not yet emitted is handled (silent for as long as it lasts) when a key of
   the same namespace, owner kind and owner name was emitted, and deferred while one is pending
   but not emitted, so a signal that never settles delays it but cannot silence it. Test the
   same-tick case, a signal that never settles, and another owner of the same name.

   **A rule that takes precedence in `detect_container`.** A container matches at most one
   detection, so a more specific reading of a state that another rule also matches gets its own
   arm, placed before the broader one: `D003` (`CrashLoopBackOff` whose last termination is
   `OOMKilled`) and `D004` (`CrashLoopBackOff` whose last termination is `StartError`) both come
   before the plain `CrashLoopBackOff` arm of `D001`. Test the precedence both ways: the specific
   last termination gives the new rule, and any other keeps the broader one. If the container's
   sample alternates between states (a waiting reason and the back-off), give the signal one
   reason in `collect_signals` and a fingerprint text that does not change with the state
   (`incident_fingerprint` in `src/incident.rs`), and test that both states give one incident and
   one fingerprint.
4. **Document it**: a row in the table of [Detection and redaction
   rules](../reference/rules.md#detection-rules) (id, name, the signal, whether it reads logs), and
   a line under `Unreleased` in [`CHANGELOG.md`](../../CHANGELOG.md). The test
   `every_id_is_in_the_rules_reference` fails until the table's ids match the code's, in order.

## Add a redaction rule

1. **Decide whether it is a new rule or a new case.** A new token format usually belongs in
   `R011` (the `KNOWN_TOKEN` pattern in `src/redact.rs`), a new secret-named key in `R010`
   (`SECRET_KEY_VALUE` and the related patterns). A new kind of value takes the next free id.
2. **Write the pattern** as a `static` `LazyLock<Regex>` in `src/redact.rs`, next to the others.
   The `regex` crate runs in linear time, so a pattern cannot be made to take exponential time by
   hostile text; keep it anchored with `\b` or explicit delimiters so that it does not eat the
   words around the value.
3. **Apply it** in `Redactor::patterns`, the one chain that both `Redactor::text` (the request)
   and `Redactor::answer` (the model's answer) run, with
   `self.apply(&PATTERN, "R0nn", out, pass, |c| …)`. The order
   is part of the rule: the specific rules run before the broad ones, so put a new specific
   pattern before `R004` (addresses), `R003`, `R013` (hostnames) and `R012` (high-entropy
   strings), which would otherwise replace the value with a less precise marker. The replacement
   is a `<redacted:…>` marker, never the value or a part of it. A pattern rule runs on the
   model's answer too, so add its id to `ANSWER_RULES` at its place in the order: the test
   `answer_rules_match_the_request_chain` fails until the list matches the ids the request pass
   counts, minus `R002`. Running a rule on requests only is a weaker redaction of the answer and
   needs an ADR ([0022. Pass the model's answer through the pattern redaction rules before it is
   written anywhere, and redact the query of an HTTP request line](../adr/0022-redact-the-answer.md)).
4. **Test it** in the `tests` module of `src/redact.rs`: lines that must be redacted (in
   `secrets_are_redacted` or `more_secret_shapes_are_redacted`, or a test of its own), lines that
   look alike and must stay (in `ordinary_lines_stay`: version strings, hashes in paths,
   timestamps), and the count in `counts_say_what_each_rule_replaced` if it is a new id. For the
   answer, add a case to `the_answer_pass_runs_every_pattern_rule` (the value is replaced and
   counted in `answer_counts()` while `counts()` does not move) and an echoed-marker case to
   `echoes_of_kausas_output_are_kept_and_not_counted` (its own marker, alone and followed by
   words, passes `answer` unchanged and uncounted).
5. **Document it**: its row in the table of [Detection and redaction
   rules](../reference/rules.md#redaction-rules), and a line in the Changelog. The test
   `every_counted_id_is_in_the_rules_reference` fails until the table lists every id that
   `src/redact.rs` counts, and no other.

A redaction rule may only make kausa send less. A change that lets through a value a rule caught
before needs an ADR.

## Add a withheld command shape

The [withheld command shapes](../reference/rules.md#withheld-command-shapes) (`W001`–`W006`) are
not redaction rules: a match replaces a whole text of the model's answer with kausa's notice in
every sink ([0025. Replace the model's text that holds one of six listed destructive command
shapes with a fixed notice, show the notice first, and give that answer `low`
confidence](../adr/0025-withhold-destructive-commands.md)). Adding, widening, narrowing or removing
one changes what an operator reads, so each needs:

1. **An ADR** that names the form, why it is destructive, and what it costs in benign answers.
2. **A corpus case**: a Deployment in `tests/bench/injection-corpus.yaml` whose log asks the model
   for the form (fake values only, busybox by its pinned digest), with its entry in
   `tests/bench/known-causes-injection.json`.
3. **The pattern**, in `PATTERNS` in `src/prompt.rs` (or a new id at the end of `SHAPES`), built
   once in the `LazyLock`; it runs on the folded copy, on every candidate (or, for prose, on the
   whole copy), with no cut.
4. **Tests** in the `tests` module of `src/prompt.rs`: the positive cases with their exact ids in
   `each_shape_matches_its_cases`, at least one **near miss** that must stay in
   `ordinary_advice_matches_no_shape`, and any new readability cost or limit pinned in
   `documented_limits_and_false_positives_are_pinned`.
5. **A replay with no benign match**: `the_committed_answers_replay` replays the committed answers,
   and the offline grading (`withhold_answers_dir` in `src/prompt.rs`, ignored by default) replays
   a measurement's; no benign answer may change.
6. **The docs**: its row in [Detection and redaction
   rules](../reference/rules.md#withheld-command-shapes), with the pattern as `Regex::as_str()`
   gives it (`|` escaped as `\|`), which the test `every_shape_is_in_the_rules_reference`
   compares byte for byte; the Limits there and in the [Security policy](../../SECURITY.md) if a
   form moves in or out of the promise; and a line in the Changelog.

## Verify

- `cargo test --locked` passes, with the new tests among them.
- The full check passes: `docker build --target ci .` ([Verification (same as
  CI)](../../AGENTS.md#verification-same-as-ci)).
- Optionally, run kausa with `--dry-run` against a test cluster with a workload that fails the
  new way ([Try kausa against a cluster with a local model](try-with-a-local-model.md)), and read
  the `incident` and `request` lines: the detection id, and the redacted text.
- For a detection rule, or a new case of one, add the failure to the known-cause corpus: a seeded
  failure in `tests/bench/seeded-failures.yaml` (invented names, busybox by its pinned digest, a
  comment with the detection and the cause), and an entry in `tests/bench/known-causes.json` with
  the cause and its rubric (`cause_all`, `first_step_any`, `confidence_ok`), written before any
  model is asked. `tests/bench/score.py` grades a model's answers against it ([Explanation quality
  of local models against known causes](../research/2026-11-02-explanation-quality.md)).

## If something fails

- **The new detection never fires** → the signal is a state but `is_state` is `false`, or it is an
  event about a kind that `detect_event` does not accept → check step 1 and step 2.
- **A redaction test sees another marker than expected** → a broader rule ran first → move the
  `apply` call earlier in `Redactor::text`.
- **A test elsewhere changed its expected output** → the new pattern matches more than intended →
  tighten it; an expected output is never updated just to make a test pass.
