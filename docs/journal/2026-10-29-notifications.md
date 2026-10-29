# 2026-10-29 — Notifications

## Request

Implement [0015. Post each new explanation to one optional webhook, as JSON, Slack or
Telegram](../adr/0015-notifications.md) on top of the namespaced mode and the explanation history
(branch `feat/v0.3-notifications`, from `feat/v0.3-history`): the code, the chart, the kind test
and the docs, with the hypotheses of the ADR checked before the bodies were written. The work
started on the evening of 2026-10-28 and ended on 2026-10-29.

## What was done

- **The code**, in two files as the ADR decides: `src/notification.rs`, the pure renderer of the
  `json`, `slack` and `telegram` bodies (the 4,000-unit cut, Slack's escaping, Telegram's chat
  id), with golden tests and a new `notification` fuzz target; and `src/notify.rs`, the transport
  (the URL checks, a `reqwest` client with no proxy and no redirects, one retry after 30 seconds,
  the queue of 16 and the dequeue rule), tested against a one-shot local server. `Redactor::
  restore_only` maps back only the namespace and the top-level owner. The deduplication cache
  keeps each entry's notification text and whether the webhook acknowledged it; `explain.rs`
  queues after the Event, and `run.rs` sends from a second slot beside the explanation. Five
  `--notify-*` flags with their usage errors, two counters, four log lines and a `notifications`
  field on `started`. No new crate.
- **The chart**: the `notifications` values and `networkPolicy.notifications`, read nil-safely;
  the arguments, the `notify` Secret volume (mode 0440, never optional), a fourth egress rule with
  a TCP 443 fallback, eight refusals in `kausa.validate`, and a `NOTES.txt` paragraph. `kausa.rules`
  is untouched. `tests/chart/render.sh` and the Dockerfile's `ci` target check each format in both
  scopes, values without the maps, a float chat id and every refusal.
- **The kind test** gained a fake receiver on `/hook/`, answered `503` first and `200` after, and
  installs with notifications in the `json` format from a Secret whose URL carries a fake
  Telegram-shaped token, with a fake bearer token.
- **The docs**: [Data sent in notifications](../reference/data-sent-in-notifications.md) and [Send
  notifications to Slack, Telegram or a webhook](../how-to/send-notifications.md), new; the
  command line, chart, permissions and provider references, ARCHITECTURE.md (flow, codemap,
  invariant 6 reworded and invariant 11), the architecture page, AGENTS.md, the Security policy,
  the README, the front page and the changelog updated; an `Update` paragraph on 0008 with the
  link it promised. 0001 already had its `Update` from 2026-10-28.
- **The fuzz workflow** now also runs when `src/notification.rs` changes, since the
  `notification` target includes it by path.

## Decisions

- [ADR 0015](../adr/0015-notifications.md) was implemented as accepted; where the task outline
  differed (one file instead of two), the ADR won.
- **Checked before the bodies were written** ([Slack and Telegram for
  notifications](../research/2026-10-28-slack-and-telegram-for-notifications.md)): three results
  differed from the ADR, and FedeAM decided each one, recorded as `Update 2026-10-29` on 0015. The
  Telegram text carries U+2060 (WORD JOINER) after every `@` and `/`, since Telegram turns plain
  `@username` and `/command` into entities and a mention notifies even a muted member; `telegram`
  sends start at least 3 seconds apart, since a bot may send 20 messages a minute to a group; and
  the `slack` body adds `"parse": "none"`, since escaping does not stop a plain URL from becoming
  a link.

## Hypotheses, settled

- **A missing Secret or key** (ADR 0015, "Hypothesis for kind"): confirmed in kind (Kubernetes
  v1.36.4). Without the Secret, without its `url` key, or without the `token` key that `tokenKey`
  names, kausa's Pod stays in `ContainerCreating` with a `FailedMount` event (`secret
  "kausa-notify" not found`, `references non-existent secret key: url`, `... key: token`); it
  never starts without notifications. Once the key was added, the same Pod started about 8 seconds
  later with no restart.
- **`--reuse-values` from an earlier chart**: values with no `notifications` map and no
  `networkPolicy.notifications` render like the chart's defaults, with no nil-pointer error
  (`helm template`, `tests/chart/render.sh`).
- **An unquoted Telegram chat id**: a values file with `telegramChatId: -1001234567890` reaches
  the template as a float and renders, with `printf "%.0f"`, as `"-1001234567890"`.
- **Still hypotheses**: that Slack incoming webhooks honour `parse`, that the joiners stop
  Telegram's mentions and commands, whether Telegram links a URL whose slashes carry a joiner, that
  the hosted Telegram Bot API returns the same HTTP status as `error_code`, and that bots cannot
  join secret chats. Nothing was posted to a real Slack workspace or Telegram chat.

## How it was checked

`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D
warnings`, `cargo test --locked --all-features` (272 unit tests; in `tests/cli.rs`, 43 on macOS and 44 on Linux, every
usage error with its exact message), `cargo deny --locked check` and `gitleaks git --redact .`
passed, and so did `docker build --target ci .` on linux/arm64 (the chart's notification renders
and refusals included). `tests/e2e/run.sh` passed in kind (Kubernetes v1.36.4) with
`KAUSA_E2E_SCOPE=cluster` and `namespaced`: the first `POST` to the fake receiver got `503`, one
`v1` body each for `crasher` (`D001`) and `failing-job` (`D008`) was answered `200`, with the real
namespace and no placeholder, context or log-tail line, every request carried the bearer token,
`requests.jsonl` held no notification, the counters showed at least 2 sent and 1 retried, kausa's
log held neither fake token, and the `kubectl auth can-i` lists were unchanged. Each run used a
kubeconfig file of its own; the maintainer's kubeconfig had the same checksum before and after,
and every kind cluster was deleted. The `notification` and `redact` fuzz targets were built with
the stable toolchain (no nightly or `cargo-fuzz` on the machine) and ran for a minute each
without coverage instrumentation, so only as random inputs: no crash. ClusterFuzzLite runs them
properly on the pull request.

## Follow-ups

- Post once to a real Slack incoming webhook and a real Telegram group before calling either
  supported; record what the joiners, `parse` and the previews do there, and add an `Update` to
  0015 if anything differs.
- ADR 0015 says notifications are in scope "from v0.2"; v0.2.0 was released before them, so they
  ship in the next release (the 0001 update of 2026-10-28 already moved them to v0.3). The wording
  is left for the maintainer.
- Slack-compatible receivers (Mattermost, Discord's `/slack` path) remain untested.

## Later the same day: v0.3.0

### Request

Merge the three v0.3 features into `main` and prepare the release change for v0.3.0, as [Maintain
the repository](../how-to/maintain.md) ("Cut a release", step 1) describes.

### What was done

- **The three features merged on GitHub** on 2026-10-29, squash-merged in the order they were
  stacked: #40, the namespaced install mode ([0013. Offer a namespaced install mode: one watch pair
  and one Role per listed namespace, and nothing
  cluster-wide](../adr/0013-namespaced-install-mode.md)), with the breaking change that the
  chart's `rbac.scope` is required; #41, the explanation history ([0014. Keep each incident's full
  explanations in a namespaced Explanation resource, off by
  default](../adr/0014-explanation-history.md)); and #42, notifications ([0015. Post each new
  explanation to one optional webhook, as JSON, Slack or Telegram](../adr/0015-notifications.md)).
  Each was built by a team of agents, as the v0.2 design was (for #41: a lead, QA-first tests,
  code and chart tracks, a review and a standards guardian); what was built and how it was checked
  is in
  [2026-10-28 — First release](2026-10-28-first-release.md) and [2026-10-29 —
  Notifications](2026-10-29-notifications.md).
- **The release change** (`chore(release): v0.3.0`): `version` 0.3.0 in `Cargo.toml` and
  `Cargo.lock` (only kausa's entry changed) and `version` and `appVersion` 0.3.0 in
  `charts/kausa/Chart.yaml`; the changelog's `Unreleased` entries moved to `[0.3.0] - 2026-10-29`,
  with a two-line summary, an empty `Unreleased` and the compare links updated.
- **The docs now say the features are in v0.3.0** instead of "in the code, not in a release yet"
  or "planned for v0.3": the README (status, the notifications paragraph and the roadmap), the
  front page, the [Security policy](../../SECURITY.md) (also its supported versions, which still
  said that nothing had been released), [Permissions](../reference/permissions.md), [Helm
  chart](../reference/helm-chart.md), [Command line and log format](../reference/command-line.md),
  [Explanation resource](../reference/explanation-resource.md), [Data sent in
  notifications](../reference/data-sent-in-notifications.md), [Send notifications to Slack,
  Telegram or a webhook](../how-to/send-notifications.md), [Install kausa with
  Helm](../how-to/install-with-helm.md), [Architecture](../explanation/architecture.md) and the
  status comment of `src/run.rs`. The statements about the v0.2.0 and v0.1.0 charts, which have no
  `rbac.scope`, stay as they were: the upgrade from them still needs `--set rbac.scope=cluster`
  once.
- **The install commands point at v0.3.0:** the README's quickstart installs
  `kausa-0.3.0.tgz` with `--set rbac.scope=cluster` and runs the crasher from
  `ghcr.io/fedeam/kausa:0.3.0`, and `CHART=` in [Install kausa with
  Helm](../how-to/install-with-helm.md) is the v0.3.0 chart. Both work only once the tag is
  pushed and the `Release` workflow has published them.

### Decisions

- **0.3.0, not 0.2.1:** the chart's required `rbac.scope` breaks every `helm upgrade` that does
  not set it, and the release adds features, so it is a minor version, as Semantic Versioning
  asks before 1.0.
- No new ADR: the release follows [0012. Release from a tag: a signed image on GHCR and the chart
  on the GitHub release](../adr/0012-releases.md).

### Still hypotheses

- **Slack and Telegram:** nothing has been posted to a real Slack workspace or Telegram chat.
  The `slack` and `telegram` bodies are checked by unit tests against the services'
  documentation only; that Slack honours `parse` and that the word joiners stop Telegram's
  mentions and commands are unverified ([Data sent in
  notifications](../reference/data-sent-in-notifications.md)).
- **Fuzzing only in CI:** the `history` and `notification` fuzz targets were never run with
  coverage instrumentation on a development machine (no nightly toolchain or `cargo-fuzz` there);
  their only proper runs are ClusterFuzzLite's in GitHub Actions.
- A hosted provider over HTTPS has still not been tried.

### How it was checked

`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D
warnings`, `cargo test --locked --all-features` (272 unit tests, 43 command-line tests on
macOS), `helm lint --strict charts/kausa --set rbac.scope=cluster` and `tests/chart/render.sh`
passed on the release branch, and the relative links of the changed files resolve.
`tests/chart/render.sh` compared the renders with notifications off with those of a local branch
by default, and the version bump changes the `app.kubernetes.io/version` and `helm.sh/chart`
labels, so those comparisons failed on the version alone in the maintainer's clone (CI, without
that branch, skipped them). `KAUSA_CHART_PREVIOUS` is now unset by default, and the comparison
runs only when it names a ref.

### Follow-ups

- Merge the release pull request, sync Gitea, tag `v0.3.0` on `main`, and follow steps 3 to 5 of
  "Cut a release": watch the `Release` workflow, check that the package stays public, and verify
  the signature, the provenance and an anonymous pull of `ghcr.io/fedeam/kausa:0.3.0`.
- Install the released v0.3.0 chart in a throwaway kind cluster, as was done for v0.1.0.
- Post once to a real Slack incoming webhook and a real Telegram group, and record the result.
- A test against a hosted provider.

## Later the same day: a real Telegram post

- The maintainer asked to test the notifications with the Telegram bot another project of theirs
  already uses. Its token and chat id stay in that project's secret store, which the agents cannot
  read; the maintainer provided the login for the test, the values went to kausa through a pipe and
  an environment variable, and nothing was printed or written to disk. The CLI was logged out
  after.
- kausa v0.3.0 (a release build on the maintainer's machine) watched a throwaway kind cluster with
  a crash-looping Deployment, used a fake model whose answer carried `@kausa_test_nobody_x`,
  `/start@kausa_nobody_bot` and a URL, and posted the `telegram` notification: `notification sent`,
  status `200`, attempt 1, about a second after the explanation.
- Two control messages sent straight to the same chat, reading the entities Telegram returned:
  without the joiners, `mention`, `bot_command` and `url`; with them, only `url`. The joiners work
  as ADR 0015 intended, and a URL stays a link (previews are off).
- Slack has still not been posted to: there is no workspace webhook to test with.

## Later the same day: D011, D012 and D013

- Three detection rules from failures reproduced in kind on 2026-10-28, all from Warning events
  kausa already watches and with no new permission: `D011`, a workload cannot create Pods
  (`FailedCreate` about a ReplicaSet, Job, StatefulSet or DaemonSet: a quota, an admission webhook
  or a policy refused them); `D012`, a PersistentVolumeClaim cannot be provisioned
  (`ProvisioningFailed`); `D013`, a HorizontalPodAutoscaler cannot get its metrics or its target.
- Built like the v0.2 rules: acceptance tests first, one developer per rule, then QA (the kind
  test in cluster scope, and a live `D011` seeded with a ResourceQuota of zero Pods), a security
  review and the standards guardian.
- The security review found that widening the redaction pattern for names quoted in event notes
  let through a `secret "<name>"` that v0.3 caught (invariant 4). The fix keeps the old pattern as
  is and adds the new kinds in a second pass, with a regression test.
- Not yet run locally: the `ci` target, for lack of space on the Docker disk; CI runs it on the
  pull request. A `D011` request can spend its ten event slots on near-identical events; noted
  for later.

## Later the same day: richer context for the model (0017)

- Request: implement [0017. Send the model resource requests, probe handlers, image pull settings
  and an unschedulable Pod's scheduling keys, reduced and
  redacted](../adr/0017-richer-context-for-the-model.md) on top of the accepted v0.4 ADRs.
- Code track (`src/record.rs`, `src/context.rs`, `src/explain.rs`, `src/prompt.rs`, and the test
  fixtures of `src/notify.rs` and `src/incident.rs`): acceptance tests first, then the record
  (requests, quantities checked against the Kubernetes grammar, the probe's handler, resolved
  port, path without its query and fragment, success threshold, pull policy, pull-secret count, and
  `SchedulingRecord` for an unschedulable Pod only, with its caps, closed `os` and `arch` values as
  a fixed-size set, and saturating counts), the context with its rule gates (`path` for `D001` and
  `D006`, pull settings for `D002`, `scheduling` for `D005`; a gated key is absent, never `null`),
  a last `fit()` step that sends `scheduling` and paths as `null` before skipping, and
  `PROMPT_VERSION` `"2"` with the new facts in the system message. `src/run.rs`, `src/redact.rs`,
  `src/fingerprint.rs`, `Cargo.toml` and `Cargo.lock` did not change.
- Chart and e2e track: nothing to do in `charts/kausa/`, `tests/chart/render.sh` or
  `.github/workflows/` (no flag, value, template, permission or workflow changes); `helm lint` and
  `tests/chart/render.sh` pass unchanged. `tests/e2e/failures.yaml` gives `crasher` a readiness
  probe with an invented query and adds the Deployment `stranded`, whose invented node selector
  and toleration no kind node matches (`D005`); `tests/e2e/run.sh` checks that `requests.jsonl`
  holds the path and the keys and never the query, the values or the Deployment's name, and
  expects three notifications.
- Docs: [Data sent to the provider](../reference/data-sent-to-the-provider.md), [Detection and
  redaction rules](../reference/rules.md), [Data sent in
  notifications](../reference/data-sent-in-notifications.md),
  [Architecture](../explanation/architecture.md), [SECURITY.md](../../SECURITY.md) and the
  [Changelog](../../CHANGELOG.md). `ARCHITECTURE.md` did not change: no invariant's text changes
  and its codemap rows still hold, as the ADR says.

### Decisions

- The fields, gates and caps are 0017's; where the working spec and the ADR could differ, the ADR
  won, and no behaviour differed.
- The e2e readiness probe has `initialDelaySeconds: 3600`, so the kubelet never probes the
  crashing container: otherwise its `Unhealthy` note for the connection error would quote the full
  URL, query included (the known gap of 0017's fact 4), into `requests.jsonl`, and could open a
  `D006` incident and an extra notification.

### How it was checked

`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D
warnings` and `cargo test --locked --all-features` (310 unit tests, 43 command-line tests, 1 other
on macOS) passed after each code commit and on the merged branch; `cargo deny --locked check`,
`cargo audit`, `gitleaks git --redact`, `helm lint --strict charts/kausa --set rbac.scope=cluster`,
`tests/chart/render.sh`, `bash -n` and `shellcheck` on `tests/e2e/run.sh` passed too, and the
relative links of the changed files resolve. The kind test was not run locally: the shared Docker
disk had 3.8 GB free, under the 4 GB this work requires, so both scopes are left to CI's `kind
(cluster)` and `kind (namespaced)` jobs on the pull request. For the same reason none of the
ADR's hypotheses was checked on a live cluster in this session.

### Follow-ups

- The benchmarks the ADR asked for before acceptance, not run here because they need the shared
  Docker disk and a real provider: `tests/bench/thousands-of-pods.py` with probes and Pods at the
  worst scheduling record, `seeded-failures.yaml` with an HTTP probe and an unschedulable
  Deployment, and the tokens measured per rule, in a dated research note. Until then the record
  size (about 6 KiB per unschedulable Pod at worst) and the token cost are estimates.
- An issue for a stricter rule on URL queries in free text (the probe URL a kubelet note quotes).
- The hypotheses the ADR leaves open: that the API validates `matchExpressions` keys and
  `topologyKey` like label keys and does not cap an affinity requirement's values, that the
  ServiceAccount admission plugin copies pull secrets into the Pod, that preferred terms never
  block scheduling, that the `os` and `arch` lists are complete, and that other Kubernetes
  versions word the scheduler's and kubelet's messages alike.
- `docs/reference/rules.md` repeats its "Which Events count" paragraph, as it did before this
  change; left for a separate fix.
