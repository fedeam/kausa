# Add a detection or redaction rule

The files to change, the tests to write and the docs to update for a new rule, or a new case of
an existing one. What a rule needs to be accepted (an id, evidence, fixtures, expected output) is
in [Contributing to kausa](../../CONTRIBUTING.md#propose-a-rule); the rules themselves are in
[Detection and redaction rules](../reference/rules.md).

Status as of 2026-10-28: ten detection rules (`D001` to `D010`) and thirteen redaction rules
(`R001` to `R013`); the paths below are the ones in the code today.

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
   with its id (`/// \`D009\`: …`), and its arms in `Detection::id` and `Detection::name`. Then
   decide, for the new variant:
   - `Detection::settles`: whether it must persist through the settle window (everything but a
     final failure such as a failed Job);
   - `Detection::is_state`: whether the signal is a state read from the Pod at every check
     (container states, a Pod condition) rather than an event;
   - `reads_logs` in `src/explain.rs`: whether the explanation needs the failed container's log
     tail. Reading logs needs `pods/log`, so a new detection that reads them is a decision about
     what kausa reads ([0009](../adr/0009-least-privilege-rbac.md)).

   `is_state` and `reads_logs` use `matches!`, so a new variant is `false` in both unless you add
   it: check both on purpose.
2. **Detect it**, in the function for its kind of signal:
   - a container state (waiting or terminated reason, exit code): `detect_container` in
     `src/detect.rs`;
   - a Pod condition: a function like `detect_unschedulable`, called from `collect_signals` in
     `src/incident.rs`;
   - a Warning event (its reason, and the kind of object it is about): `detect_event` in
     `src/detect.rs`.

   If the rule needs a field kausa does not record yet, it goes into the small records of
   `src/record.rs` first. A field that would reach the model's request is a decision about data
   leaving the cluster: it needs an ADR and an update to [Data sent to the
   provider](../reference/data-sent-to-the-provider.md) in the same change.
3. **Test it** in the `tests` module of `src/detect.rs`: the status or event that triggers the
   rule, and the closest ones that must not (a single restart during a rollout, a `Completed`
   container, an event about another kind). If the rule changes how incidents are grouped or when
   they settle, add a case to the tests of `src/incident.rs` as well.
4. **Document it**: a row in the table of [Detection and redaction
   rules](../reference/rules.md#detection-rules) (id, name, the signal, whether it reads logs), and
   a line under `Unreleased` in [`CHANGELOG.md`](../../CHANGELOG.md).

## Add a redaction rule

1. **Decide whether it is a new rule or a new case.** A new token format usually belongs in
   `R011` (the `KNOWN_TOKEN` pattern in `src/redact.rs`), a new secret-named key in `R010`
   (`SECRET_KEY_VALUE` and the related patterns). A new kind of value takes the next free id.
2. **Write the pattern** as a `static` `LazyLock<Regex>` in `src/redact.rs`, next to the others.
   The `regex` crate runs in linear time, so a pattern cannot be made to take exponential time by
   hostile text; keep it anchored with `\b` or explicit delimiters so that it does not eat the
   words around the value.
3. **Apply it** in `Redactor::text`, with `self.apply(&PATTERN, "R0nn", out, |c| …)`. The order
   is part of the rule: the specific rules run before the broad ones, so put a new specific
   pattern before `R004` (addresses), `R003`, `R013` (hostnames) and `R012` (high-entropy
   strings), which would otherwise replace the value with a less precise marker. The replacement
   is a `<redacted:…>` marker, never the value or a part of it.
4. **Test it** in the `tests` module of `src/redact.rs`: lines that must be redacted (in
   `secrets_are_redacted` or `more_secret_shapes_are_redacted`, or a test of its own), lines that
   look alike and must stay (in `ordinary_lines_stay`: version strings, hashes in paths,
   timestamps), and the count in `counts_say_what_each_rule_replaced` if it is a new id.
5. **Document it**: its row in the table of [Detection and redaction
   rules](../reference/rules.md#redaction-rules), and a line in the Changelog.

A redaction rule may only make kausa send less. A change that lets through a value a rule caught
before needs an ADR.

## Verify

- `cargo test --locked` passes, with the new tests among them.
- The full check passes: `docker build --target ci .` ([Verification (same as
  CI)](../../AGENTS.md#verification-same-as-ci)).
- Optionally, run kausa with `--dry-run` against a test cluster with a workload that fails the
  new way ([Try kausa against a cluster with a local model](try-with-a-local-model.md)), and read
  the `incident` and `request` lines: the detection id, and the redacted text.

## If something fails

- **The new detection never fires** → the signal is a state but `is_state` is `false`, or it is an
  event about a kind that `detect_event` does not accept → check step 1 and step 2.
- **A redaction test sees another marker than expected** → a broader rule ran first → move the
  `apply` call earlier in `Redactor::text`.
- **A test elsewhere changed its expected output** → the new pattern matches more than intended →
  tighten it; an expected output is never updated just to make a test pass.
