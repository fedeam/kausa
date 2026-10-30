# 0015. Post each new explanation to one optional webhook, as JSON, Slack or Telegram

- Status: accepted
- Date: 2026-10-28
- Builds on: [0001. Build a small, read-only explainer that writes one redacted explanation per
  incident](0001-scope-and-positioning.md), [0008. Send only allowlisted, redacted data to one
  OpenAI-compatible endpoint](0008-data-leaving-the-cluster.md), [0009. Run with read-only
  permissions that never include Secrets](0009-least-privilege-rbac.md), and the two other v0.2
  ADRs proposed in the same pull request, [0013. Offer a namespaced install mode: one watch pair
  and one Role per listed namespace, and nothing cluster-wide](0013-namespaced-install-mode.md)
  (issue #14) and [0014. Keep each incident's full explanations in a namespaced Explanation
  resource, off by default](0014-explanation-history.md) (issue #13)
- Amends: 0001 (two lines of its out-of-scope list) and 0008 ("exactly one destination"); both
  keep the status `accepted` and get an `Update` paragraph when this ADR is accepted

## Context

Status as of 2026-10-28 (v0.1.0): each explanation goes to an `Explained` Event (note cut at 1024
bytes) and to kausa's log, and Events last one hour by default (`--event-ttl`), so an explanation
written at night is gone by morning. History (0014) still has to be looked at. On 2026-10-28 FedeAM
decided that v0.2 adds notifications: a documented JSON webhook, Slack-compatible incoming webhooks
and Telegram; real names (the destination is the maintainer's own channel), the explanation, the
object and the rule; never logs, never the context sent to the model. The three v0.2 ADRs are
reviewed in one docs pull request before any code, and implemented in the order 0013, 0014, 0015.

0001 puts "notification sinks" and "any network call other than the Kubernetes API and the
configured LLM endpoint" out of scope, and 0008 sends redacted, pseudonymous data to "exactly one
destination", as AGENTS.md and the [Security policy](../../SECURITY.md) repeat. Threat model: anyone
who can make a workload fail in a watched namespace controls the facts behind a notification, and
the model its text. The webhook URL is a credential (Telegram puts the bot token in the path). The
receiver keeps what it gets under its own retention. The new risks are the URL leaking, cluster text
reaching a chat service, crafted text abusing chat features (mentions, disguised links, previews), a
tenant flooding the channel, and a slow webhook stalling kausa.

## Decision

kausa can post each new explanation to **one** webhook, off by default. The message carries only the
fields below, with the real names of the namespace and the object; every other name stays a
placeholder. It is sent by its own future, outside the explanation path, with no Kubernetes
permission and no new crate, and acknowledged notifications are bounded by the call budget.

**When.** Only `Explained` outcomes: a skip is already an Event, a log line and a counter, and skips
are not bounded by the budget. Once per explanation the model wrote: the `Dedup` entry gains
`notified` (false at first) and, with notifications on, the notification's version of the
explanation, so a cached repeat sends the same text and queues only while `notified` is false.
`notified` is set only after a successful send. At dequeue, kausa looks the entry up without the TTL
or removal and skips the send (not counted, not logged) only if the entry stores the same
explanation and is `notified`; after a success it marks the entry only if it still stores that
explanation, so an evicted entry's notification is still sent and an older send never marks a newer
entry. The window is `--dedup-hours` (no new flag; with `0`, every occurrence is notified). It is
queued after `emit` returns, so `--no-events` with notifications is valid. `--dry-run` with
notifications is a usage error, and the chart refuses it. **Rate bound:** notifications acknowledged
with a `2xx` ≤ model calls ≤ `--max-calls-per-hour` and `--max-calls-per-day` (30 and 200 by
default); no budget of its own. After a failure, one attempt per occurrence plus its retry, and an
incident recurs only after 330 s without its failure. A receiver that answers after the 10 s timeout
gets at most one duplicate per retry and per occurrence, roughly 60 per entry over the 6 h default.

| Flag (`kausa run`) | What it does |
|---|---|
| `--notify-url-file <PATH>` | Turns notifications on. The file, trimmed, is the full webhook URL, a secret in every format, read once at start. No flag value or environment variable form |
| `--notify-format <json\|slack\|telegram>` | The body; `json` by default |
| `--notify-telegram-chat-id <ID>` | Required with `telegram` only: an integer (may be negative) or `@` and a channel's username. Not a credential |
| `--notify-token-file <PATH>` | `json` only, optional: sent as `Authorization: Bearer <token>` |
| `--notify-allow-http` | Allows `http://` for a receiver in the cluster or private network; separate from `--allow-http` |

Usage errors (exit code 2): a `--notify-*` flag without `--notify-url-file`; that flag without
`--endpoint` or with `--dry-run`; a missing, unreadable or empty file; an unparsable URL, a scheme
other than `https://` (or allowed `http://`), or user information; for `telegram`, a missing or
malformed chat id or a path other than `/bot<token>/sendMessage` (any host); the chat id or token
flag with another format. Errors name the flag and path, not the content; no test message at start.

**What is sent** (`content-type: application/json`, `user-agent: kausa/<version>` for `json`):
`schema` (`kausa.fedeam.com/notification/v1`), `type` (always `explained` in v1), `kausa_version`,
`detection` (`id`, `name`), `object` (the top-level owner, as in the Event's `regarding`:
`apiVersion`, `kind`, `namespace`, `name`, no UID), `fingerprint` (already in Event names),
`first_seen` (RFC 3339, UTC) and `explanation` (`summary`, `cause`, `steps`, `confidence`). Within
`v1` fields are only added and receivers ignore unknown ones; anything else is `v2`, announced in
the changelog. Duplicates can be dropped by `fingerprint` and `first_seen`.

**Names.** A narrower restore, `Redactor::restore_only` (through `Explanation::map_clean`, so the
caps hold), maps back only the namespace and the top-level owner, a bare Pod owner included. Pods,
containers, nodes, service accounts and private images stay placeholders (`node-1`,
`registry-1/image-1`): node and registry names are often internal hostnames. The Event and the log
keep the full restore. **Never sent:** log lines, event or status messages, `sample.message`, the
request or context, the pseudonym mapping, other real names, labels, annotations, redaction and
token counts, the prompt version, the model's name, the endpoint, the API key, the configuration.
Redacted values stay redacted. "Never logs" means no field carries a log line: the model's text can
still repeat redacted log or event text, and kausa cannot tell a quote from a summary.

**Text.** `slack` and `telegram` share one plain-text rendering (a header such as `kausa: crash loop
(D001) in shop, Deployment web`, the summary, `Likely cause:`, numbered `Check:` steps, `Confidence:
… · fingerprint …`), cut at a character boundary to 4,000 UTF-16 code units, then escaped for
`slack` (at most 20,000 units). `slack` sends `{"text", "mrkdwn": false, "unfurl_links": false,
"unfurl_media": false}` with `&`, `<` and `>` escaped, so neither the `<redacted:…>` markers nor any
text become a mention or a link; `telegram` sends `{"chat_id", "text", "link_preview_options":
{"is_disabled": true}}` with no `parse_mode`. Hypotheses to check before implementing: Telegram's
4,096 limit and unit, Slack's truncation at 40,000, the three escapes, that webhooks honour `mrkdwn`
and `unfurl_*`, the Telegram field names, and whether plain `@here`, @mentions, URLs or commands
become mentions or entities (if so, an `Update` with the fix). Slack-compatible receivers
(Mattermost, Discord's `/slack` path) are hypotheses; a receiver is supported only after a test,
Slack and Telegram once posted to with a real webhook. The rendering is a pure module,
`src/notification.rs` (`panic = "abort"`, and a tenant shapes its input), with a `notification` fuzz
target: no panic, the length caps, no `<` or `>` in the Slack text, a `json` body that parses. The
`redact` target also calls `restore_only`. `src/notify.rs` is the transport.

**Transport.** A `reqwest::Client` like the model's (rustls, `no_proxy()`, no redirects,
`https_only` unless allowed), 5 s connect and 10 s total timeouts. Only the status code is read:
`2xx` succeeds, `3xx` fails (the log says to put the final URL in the file). One retry 30 s later
for a timeout, a failed connection, `429` or `5xx`; other statuses fail at once; `Retry-After` is
ignored; about 50 s at worst. One send at a time, at least 1 s apart, from an in-memory queue of 16
that drops its oldest, driven by `run.rs` beside the watches. A failure never delays the watches, an
explanation or an Event, never stops kausa or changes its exit code, and is never an
`ExplanationSkipped` Event. Queued sends are dropped at `SIGTERM`. The URL and token never appear in
logs, errors, Events or labels. Hypotheses: Telegram's HTTP status follows `error_code` (otherwise
`ok` is parsed, fuzzed, with an `Update`); both allow about one message per second.

**Chart.** `notifications.enabled` (`false`), `format` (`json`), `existingSecret` (required),
`urlKey` (`url`), `tokenKey` (`""`), `telegramChatId` (`""`), `allowHttp` (`false`), and
`networkPolicy.notifications.to` and `.ports` (TCP 443 when missing or empty, never every port). The
Secret is mounted read-only at `/var/run/secrets/kausa-notify/` (`defaultMode: 0440`, through
`fsGroup`). The chart refuses to render with an empty `existingSecret`, an unknown `format`,
`telegram` without `telegramChatId`, `telegramChatId` or `tokenKey` with the wrong format, no
`model.endpoint`, `dryRun`, or the NetworkPolicy on with an empty `to`. The NetworkPolicy gets a
fourth egress rule; it cannot select a hostname, so hosted receivers need an `ipBlock`, FQDN
policies or an egress proxy (stable Telegram ranges are a hypothesis). `default dict` lets v0.1
values kept by `--reuse-values` render (hypothesis: otherwise a nil-pointer error). `NOTES.txt` says
how to find failures. No permission is added in either mode; one namespaced release per team can
have its own channel. Hypothesis for kind: a missing Secret gives `FailedMount`, not a silent start.

**Logs and metrics.** `started` gains `notifications: {"format", "host"}` or `null`. Lines:
`notification sent` (info; `fingerprint`, `namespace`, `owner`, `format`, `status`, `attempt`),
`notification retried` (warn; adds `retry_in_seconds`), `notification failed` and `notification
dropped` (warn; `fingerprint`, `reason`). Counters: `kausa_notifications_total` (`outcome`: `sent`,
`failed`, `dropped`) and `kausa_notifications_retried_total`; alert on
`increase(kausa_notifications_total{outcome="failed"}[1h]) > 0`. Events, history and notifications
are independent outputs, so they need no CRD; the history object's name can join `v1` later.

**Texts the implementation pull request copies**, with the new page "Data sent in notifications"
(`docs/reference/data-sent-in-notifications.md`):
- **0001** `Update`: one-way notifications to one webhook are in scope from v0.2; interactive chat
  and a user interface stay out. **0008** `Update`: the model endpoint stays the only destination of
  cluster data; an optional second one receives only the fields of Data sent in notifications, with
  real names of the namespace and the object. Invariants 2, 3 and 4 do not change.
- **Invariant 6:** "One call per incident, within the budget, and one explanation at a time; nothing
  the cluster, the endpoint or the webhook does may stop the watches, and a notification never
  delays an explanation or an Event."
- **New invariant 11:** "Nothing reaches the webhook but the fields of Data sent in notifications:
  no field holds a log line, an event message or any part of the context sent to the model, and no
  real name but the namespace's and the object's. The webhook URL and token are secrets read from
  files, never logged."
- **AGENTS.md** and the **Security policy:** nothing unredacted reaches the model, and nothing
  reaches any host but the API server, the endpoint and the configured webhook (only the fields of
  Data sent in notifications, with real names); the URL and token sit next to the API key; "What
  kausa cannot promise" adds what the destination learns, repeated log text, mentions and links.

## Alternatives considered

- **Notify from the history resource:** needs `list` and `watch`, loses a notification when a
  history write fails, and re-notifies on relist. A persistent mark there can come on top of 0014.
- **Forward Events with an existing tool** (Robusta, kwatch, an event exporter): only the cut note,
  read before it expires, by another component that reads every Event. Still valid elsewhere.
- **Post inline in `explain::run`:** a dead webhook holds the explanation slot up to 50 s per
  incident (invariant 6). **Own budget and repeat window:** the budget and `notified` cover both.
- **Never notify a cached repeat** hides a failed send; **every occurrence** floods the channel.
- **Fixed Telegram host with a token file:** a second secret path, no self-hosted Bot API, no fake
  receiver without a test-only override. **Chat id in the query:** unverified behaviour.
- **The URL in a flag, value or environment variable:** visible in the Pod spec and process lists.
  **Logging the webhook's answer:** third-party text in the log; the status is enough.
- **Pseudonyms for the object:** useless to the reader. **The full restore:** internal names, beyond
  "the object and the rule". **Extra fields, several webhooks, templates, Block Kit, Telegram
  markup, HMAC:** more surface; HMAC needs a crate or hand-rolled crypto. Each can come later.

## Consequences

- Real names leave the cluster for the first time, with text that may repeat redacted logs, to a
  third party under its own retention; Telegram bot chats are not end-to-end encrypted (hypothesis).
  "Fully local" now also means notifications off or an in-cluster receiver. A wrong URL sends
  explanations to its owner; a leaked one lets anyone post as kausa. The fingerprint, a
  non-cryptographic hash that includes the sample message, leaves too (already in Event names); the
  `src/fingerprint.rs` comment becomes "never sent to the model". A notification can still mislead,
  mention a member or show a clickable link (hypotheses above).
- Slow receivers can get duplicates; one tenant can push out another's notification; a restart
  notifies still-failing incidents once; a proxy-only cluster cannot use notifications yet. Cost:
  one mounted Secret, five flags, two counters, four log lines, one fuzz target, and per `Dedup`
  entry a boolean and up to 2,400 characters (a few megabytes at 1,024 entries).
- **Docs:** the new reference page, linked from [Data sent to the
  provider](../reference/data-sent-to-the-provider.md); a how-to, "Send notifications to Slack,
  Telegram or a webhook" (`example.com`, fake tokens); updates to [Command line and log
  format](../reference/command-line.md), [Helm chart](../reference/helm-chart.md),
  [Permissions](../reference/permissions.md), [Architecture](../explanation/architecture.md),
  ARCHITECTURE.md, AGENTS.md, the Security policy (two new vulnerabilities: a request to another
  host, and an undocumented field, log text or a URL or token in a log line), README, CHANGELOG.
- **Tests:** golden bodies (`json` equal to the reference example), escaping, caps, a log canary
  absent from every body, URL, chat id and token-leak checks; a local server for each status, a
  timeout and the bearer header; `restore_only`; the dequeue rule; the fuzz target; every usage
  error in `tests/cli.rs`; chart renders per format, with v0.1 values and each refusal; e2e against
  a fake receiver (`503` then `200`) asserting one `v1` notification each for `crasher` and
  `failing-job`, no context or placeholder in any body, Events despite the `503`, the counters, no
  token in the log and unchanged `kubectl auth can-i`. Slack and Telegram are not tested end to end
  (CI has no secrets); the log check holds only because the fake model never echoes its input.

Update 2026-10-29: [Slack and Telegram for
notifications](../research/2026-10-28-slack-and-telegram-for-notifications.md) checked this ADR's
hypotheses against Slack's and Telegram's documentation, and found three things that differ from
what it assumed; FedeAM decided each one. First, Telegram turns `@username`, `/command` and
`/command@bot` in plain text into entities, and a mention notifies a member even in a muted group,
so a tenant could make the model's text ping someone or reach another bot. The Telegram text, and
only it, now carries U+2060 (WORD JOINER) after every `@` and every `/`: it looks the same, and
should leave Telegram no mention, bot command or `/command@bot` to detect (a hypothesis until the
first real post). The joiners are added after the cut, and the cut counts them, so the text, joiners
included, stays within 4,000 UTF-16 units; with many `@` or `/` it is cut earlier than Slack's.
Slack's text does not need them, since kausa never sends `link_names`. Whether Telegram still
detects a URL whose slashes carry a joiner was not checked. Second, Telegram lets a bot send 20
messages a minute to a group, not one a second, so `telegram` sends start at least 3 s apart; `json`
and `slack` keep 1 s, Slack's documented rate for incoming webhooks. Third, escaping keeps Slack's
mentions inert but a plain URL still becomes a link, so the `slack` body adds `"parse": "none"`;
Slack does not say that webhooks honour it, and one that ignores it is no worse off. The other
results, and what stays a hypothesis, are in the note; nothing has been posted to a real Slack
workspace or Telegram chat yet.

Update 2026-10-29, later: On 2026-10-29 kausa v0.3.0, run from a release build against a kind
cluster with a fake model, posted a `telegram` notification to a real Telegram chat through a real
bot: the Bot API answered `200`. Two control messages sent straight to the same chat showed what
Telegram detects: without the joiners, a mention, a bot command and a URL; with them, only the URL.
The joiners do keep a `@username` from being a mention and a `/command@bot` from being a command; a
URL whose slashes carry a joiner is still a link (previews stay off). Slack has still not been
posted to.

Update 2026-10-30: kausa v0.4.0, run the same way, posted one `slack` notification to a real
incoming webhook: Slack answered `200`. The model's answer carried `@here`, `@channel`,
`<!everyone>`, `<!channel>` and a URL. The URL was plain text, so the webhook honours `"parse":
"none"`. The escaped `<!everyone>` and `<!channel>` showed as `!everyone` and `!channel`. But the
plain `@here` and `@channel` became mentions that notified the channel, although kausa sends no
`link_names` and asks for `"parse": "none"`: the documentation read on 2026-10-28 did not hold for
webhooks. The `slack` text therefore gets the Telegram fix: a word joiner after every `@`, added
after the cut, which the cut counts. A second post the same day, from a build with the joiner,
showed `@here`, `@channel` and `@everyone` as plain text, not as mentions.
