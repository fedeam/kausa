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
