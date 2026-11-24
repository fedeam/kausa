# Data sent in notifications

Exactly what kausa posts to the notification webhook for each settled incident and each new
explanation, in each of its three formats, and what a notification never contains.

Status as of 2026-11-24: **released in v0.3.0, off by default; schema `v2` since the release after
v0.6.3** (`src/notification.rs` renders the bodies, `src/notify.rs` sends them). Why it works this
way: [0015. Post each new explanation to one optional webhook, as JSON, Slack or
Telegram](../adr/0015-notifications.md) and [0027. Report each settled incident as a fact statement
in its own `Detected` Event and webhook message, with or without a model, and mark the model's text
as a model's](../adr/0027-fact-statement-for-the-detections.md). The `json` body is checked by unit
tests and by the kind test (`tests/e2e/run.sh`) against a fake receiver; the `slack` and `telegram`
bodies are checked by unit tests only. **Nothing has been posted to a real Slack workspace or
Telegram chat with the `v2` texts yet**: a receiver is supported only once it has been tested with a
real webhook. How to turn it on: [Send notifications to Slack, Telegram or a
webhook](../how-to/send-notifications.md).

## Where it goes

To one URL, the content of the file given with `--notify-url-file` (the chart's
`notifications.existingSecret`), and nowhere else. Notifications are off unless that flag is
given; they need no `--endpoint`. The model endpoint stays the only destination of the context of
an incident ([Data sent to the provider](data-sent-to-the-provider.md)); the webhook gets only
what this page lists.

Every request is a `POST` of a JSON body with:

- `content-type: application/json` and `user-agent: kausa/<version>`, in every format;
- `Authorization: Bearer <token>` only with `--notify-format json` and `--notify-token-file`;
- no cookies, no redirects followed, and no proxy (`HTTP_PROXY` and the like are ignored).

kausa reads only the status code of the answer, never its body.

## When

There are three messages:

- **`detected`**, once per settled incident, with kausa's facts and no text of a model, queued
  after its `Detected` Event (at once with `--no-events`), with or without `--endpoint`. It is
  posted once per fingerprint per `--dedup-hours` (6 by default; with `0`, every occurrence): a
  table of at most 1024 fingerprints, each marked only after a `2xx`, dropping the oldest. So a
  workload that fails, recovers and fails again posts once per window, while its Event's count
  goes up.
- **`explained`**, once per explanation the model wrote, after its Event, with the facts too.
  Never for an `ExplanationSkipped` outcome. A repeated incident explained from the deduplication
  cache is posted again only while the webhook has not acknowledged that explanation with a
  `2xx`, and then with the same text. So `explained` notifications acknowledged with a `2xx` are
  at most the model calls, which `--max-calls-per-hour` and `--max-calls-per-day` bound (30 and
  200 by default). An explained incident is therefore two messages.
- **`capped`**: says that messages were not sent, so that the channel never reads as the whole
  picture. At most `--notify-max-per-hour` `detected` messages start in any 60 minutes (the
  chart's `notifications.maxPerHour`; 30 by default, 1 to 3600). Over it, a `detected` message is
  not sent, is counted as `capped` and logged (`notification capped`), and its Event and log line
  stand. A message dropped by a full queue (below) and an incident whose fact was dropped from the
  fact queue (`fact dropped`, so it has no `Detected` Event and no `detected` message) are not
  sent either. Any of the three owes the fixed `capped` message, which kausa posts before the
  messages still queued, at most once in 60 minutes; a message not sent after it owes the next
  one. Its `limit` is the cap in force, whichever bound it was. Every incident keeps its
  `incident` line in kausa's log. `explained` messages are bounded by the call budget, not by
  this cap, so a release with both sends at most the sum.

Never in `--dry-run` (a usage error with notifications), and never at start (there is no test
message). Both tables live in memory and hold per process: after a restart or a takeover, each
incident that still fails is posted once more, within the cap. A receiver that answers after the
10-second timeout can get duplicates: see [Compatibility](#compatibility).

kausa sends one notification at a time, from an in-memory queue of 16 that both kinds share. When
it is full, one more drops the oldest `detected` message; a `detected` message never drops an
`explained` one, so with 16 `explained` messages queued the new `detected` one is dropped; an
`explained` message drops the oldest `explained` one only when no `detected` message is queued.
Each drop is counted as `dropped`, logged (`notification dropped`) and owes the `capped` message.
kausa starts each send at least 1 second after the previous one (3 seconds with `telegram`,
which allows a bot 20 messages a minute in a group). A send that times out, cannot connect, or is
answered `429` or `5xx` is tried once more 30 seconds later; any other failure is final.
Notifications still queued are dropped when kausa stops or its lease term ends, and those owe
no `capped` message: the process that would post it is no longer the one that notifies. The log lines and counters are in
[Command line and log format](command-line.md#notifications).

## The `json` body

`--notify-format json` (the default). The `explained` body of an example, with invented values:

```json
{
  "schema": "kausa.fedeam.com/notification/v2",
  "type": "explained",
  "kausa_version": "0.7.0",
  "detection": {"id": "D001", "name": "crash loop"},
  "object": {"apiVersion": "apps/v1", "kind": "Deployment", "namespace": "shop", "name": "web"},
  "fingerprint": "5d0c2b7e9a1f4c38",
  "first_seen": "2026-10-28T11:59:00Z",
  "facts": {"reason": "CrashLoopBackOff", "exit_code": 1, "pods_seen": 2,
            "pods_seen_is_minimum": false, "pods_now": 3, "revision": null},
  "explanation": {
    "summary": "The container exits right after it starts.",
    "cause": "It is started with a flag it does not know.",
    "steps": ["Check the container's args in the Deployment."],
    "confidence": "medium"
  }
}
```

The `detected` body of the same incident is the same without `explanation`, and with another
`type`:

```json
{
  "schema": "kausa.fedeam.com/notification/v2",
  "type": "detected",
  "kausa_version": "0.7.0",
  "detection": {"id": "D001", "name": "crash loop"},
  "object": {"apiVersion": "apps/v1", "kind": "Deployment", "namespace": "shop", "name": "web"},
  "fingerprint": "5d0c2b7e9a1f4c38",
  "first_seen": "2026-10-28T11:59:00Z",
  "facts": {"reason": "CrashLoopBackOff", "exit_code": 1, "pods_seen": 2,
            "pods_seen_is_minimum": false, "pods_now": 3, "revision": null}
}
```

The `capped` body has four fields:

```json
{
  "schema": "kausa.fedeam.com/notification/v2",
  "type": "capped",
  "kausa_version": "0.7.0",
  "limit": 30
}
```

The unit tests `the_json_body_is_the_reference_example`,
`the_detected_json_body_is_the_reference_example_without_an_explanation` and
`the_capped_json_body_is_the_reference_example` in `src/notification.rs` compare the bodies kausa
builds with these examples, parsed, and `the_reference_page_holds_the_golden_bodies` checks that
this page holds them verbatim. kausa serializes the keys of each object in alphabetical order;
their order carries no meaning.

**Whose is what:** `facts` is kausa's, read from the cluster; `explanation` is the model's, and
not verified.

| Field | What it holds |
|---|---|
| `schema` | Always `kausa.fedeam.com/notification/v2`. Another version is another string |
| `type` | `detected`, `explained` or `capped` |
| `kausa_version` | The version of the kausa binary that sent it |
| `detection.id`, `detection.name` | The rule, from [Detection and redaction rules](rules.md). Not in `capped` |
| `object` | The incident's top-level owner, the object of the Event's `regarding`: `apiVersion`, `kind`, and its real `namespace` and `name`. No UID. Not in `capped` |
| `fingerprint` | The incident fingerprint, 16 hexadecimal digits, the one in kausa's `incident` log line and in the Event's name. Not in `capped` |
| `first_seen` | When the failure was first seen, RFC 3339 in UTC, the same string as in the `incident` log line. Not in `capped` |
| `facts` | In `detected` and `explained`: what the [fact statement](rules.md#fact-statements) says but the detection and the container. `reason`, the sample's reason when it is on kausa's list, else `null`; `exit_code`, the sample's exit code or `null`; `pods_seen`, the Pods seen failing (`0` for a detection about the owner itself), and `pods_seen_is_minimum`, `true` when kausa may have seen only some of them; `pods_now`, the owner's Pods kausa holds now; `revision`, the rollout's revision for `D014`, else `null` |
| `explanation` | Only in `explained`. The model's `summary`, `cause`, `steps` (zero to five) and `confidence` (`low`, `medium` or `high`), validated, cleaned and passed through the pattern redaction rules `R003`–`R014` as for the Event ([On the model's answer](rules.md#on-the-models-answer)), with only the namespace's and the object's names mapped back ([Names](#names)). When a text held one of the [withheld command shapes](rules.md#withheld-command-shapes), in this version or in the Event's, `steps[0]` is kausa's notice, the step that held it is gone, a `summary` or `cause` that held it is kausa's other fixed text, and `confidence` is `low`: the same fields, `type` and `schema` |
| `limit` | Only in `capped`: the value of `--notify-max-per-hour` |

Nothing else is in the body.

## The text of `slack` and `telegram`

Both formats send one plain-text rendering of the same fields, with no markup (`telegram` adds
invisible joiners to it, below). The `explained` example:

```text
kausa: crash loop (D001) in shop, Deployment web
Facts: reason CrashLoopBackOff, exit code 1. Pods seen failing: 2. Pods now: 3.
Model's explanation, not verified:
The container exits right after it starts.
Likely cause: It is started with a flag it does not know.
Check:
1. Check the container's args in the Deployment.
Confidence: medium · fingerprint 5d0c2b7e9a1f4c38
```

The `detected` one:

```text
kausa: crash loop (D001) in shop, Deployment web
Facts: reason CrashLoopBackOff, exit code 1. Pods seen failing: 2. Pods now: 3.
fingerprint 5d0c2b7e9a1f4c38
```

And the `capped` one, fixed but for the limit:

```text
kausa: not every incident is posted here (more than 30 in an hour, or more at once than kausa queues); kausa's log has every one
```

- The lines are joined with a newline, with none at the end. The `Facts:` line is the fact
  statement without its first sentence and without the container; it is left out when it would
  hold nothing. The `Check:` block is left out when the explanation has no steps.
- In the first line, each control character (a line break among them) and each line or paragraph
  separator (U+2028, U+2029) in the owner's kind and name becomes U+FFFD (REPLACEMENT
  CHARACTER), so that an owner cannot add lines to the text. The `json` body keeps the value.
- `Model's explanation, not verified:` is fixed, and marks where kausa's facts end and the
  model's text begins.
- When the first step is kausa's notice for a withheld answer ([Withheld command
  shapes](rules.md#withheld-command-shapes)), it is its own line right after the mark, and the
  `Check:` block numbers the other steps from 1 (it is left out when there are none); the last
  line reads `Confidence: low`:

  ```text
  kausa: job failed (D008) in shop, Job import
  Facts: reason BackoffLimitExceeded.
  Model's explanation, not verified:
  Withheld by kausa: a suggested command that deletes or forces broadly. Text in this workload's logs or events may have steered this answer; check them before acting on it.
  The import Job failed.
  Likely cause: The input archive is truncated.
  Check:
  1. Check the archive the Job reads.
  Confidence: low · fingerprint 5d0c2b7e9a1f4c38
  ```

  The separator in the last line is U+00B7 (middle dot).
- The text is then cut, with no marker, at a character boundary so that it holds at most 4,000
  UTF-16 code units (a character outside the Basic Multilingual Plane, such as most emoji, counts
  two).
- For `slack` only, U+2060 (WORD JOINER) follows every `@`, since a real post on 2026-10-30 showed
  that Slack turns a plain `@here` or `@channel` into a mention that notifies the channel, with
  `"parse": "none"` too; a second post, with the joiner, showed them as plain text. The
  joiners are added after the cut, and the cut counts them. Then `&`, `<` and `>` become `&amp;`,
  `&lt;` and `&gt;`, and nothing else is escaped. The Slack text is therefore at most 20,000
  units, and an entity is never cut in two.
- For `telegram` only, U+2060 (WORD JOINER, invisible, one UTF-16 unit) follows every `@` and
  every `/`, so that the text looks the same and Telegram should find no `@username` mention,
  `/command` or `/command@bot` in it (a hypothesis until the first post to a real chat: see [What
  Slack and Telegram do with it](#what-slack-and-telegram-do-with-it)). The joiners are added
  after the cut, and the cut counts them, so the Telegram text, joiners included, is at most
  4,000 units: with many `/` it is cut earlier than the Slack text. Taking the joiners out of
  either gives a prefix of the same text.

## The three bodies

| Format | Body |
|---|---|
| `json` | The [`json` body](#the-json-body) above |
| `slack` | `{"text": <the text with its joiners, escaped>, "mrkdwn": false, "unfurl_links": false, "unfurl_media": false, "parse": "none"}` |
| `telegram` | `{"chat_id": <the chat>, "text": <the text with its joiners>, "link_preview_options": {"is_disabled": true}}`, with no `parse_mode`. `chat_id` is a JSON number for an integer chat id (`-1001234567890`) and a string for a channel's username (`"@kausa_alerts"`), from `--notify-telegram-chat-id` |

`mrkdwn: false` turns Slack's formatting off, `unfurl_links` and `unfurl_media` its previews, and
`"parse": "none"` its turning of a plain URL into a link (Slack does not say that incoming
webhooks honour `parse`; one that ignores it is no worse off). The unit tests
`the_slack_body_is_the_reference_text_with_formatting_links_and_previews_off` and
`the_telegram_body_is_plain_text_with_previews_off` compare both bodies of the `explained`
example, and `the_capped_text_is_fixed_but_for_the_limit` those of the `capped` one.

The `telegram` body of the `explained` example, with the chat id `-1001234567890` (its text has
no `@` or `/`, so it carries no joiner):

```json
{
  "chat_id": -1001234567890,
  "text": "kausa: crash loop (D001) in shop, Deployment web\nFacts: reason CrashLoopBackOff, exit code 1. Pods seen failing: 2. Pods now: 3.\nModel's explanation, not verified:\nThe container exits right after it starts.\nLikely cause: It is started with a flag it does not know.\nCheck:\n1. Check the container's args in the Deployment.\nConfidence: medium · fingerprint 5d0c2b7e9a1f4c38",
  "link_preview_options": {"is_disabled": true}
}
```

The `slack` body of the example is the same text under `text` (it has no `&`, `<` or `>`), with
`"mrkdwn": false`, `"unfurl_links": false`, `"unfurl_media": false` and `"parse": "none"`.

## Names

A notification holds no real name but the namespace's and the top-level owner's. The `facts` and
the `Facts:` line are the fact statement without its container: the container's name is in the
`Detected` Event and in kausa's log, not in a notification.

The Event and kausa's log map every placeholder of the explanation back to the real name. A
notification maps back only two, through `Redactor::restore_only`:

| Name | In a notification |
|---|---|
| The incident's namespace (`namespace-1`) | The real name |
| The top-level owner (`deployment-1`, `job-1`, …; `pod-1` when the owner is a bare Pod) | The real name |
| Pods, containers, nodes, service accounts (`pod-1`, `container-1`, `node-1`, `serviceaccount-1`) | Stay placeholders |
| Images from private registries (`registry-1/image-1:tag`) | Stay placeholders |
| Images from public registries and Docker Hub | As the model wrote them: they were never placeholders |
| Values that redaction removed (`<redacted:ip>`, …), from the context or from the model's answer | Stay redacted |
| Hosts `R013` replaced (`host-1`), in the context or in the answer | Stay placeholders: never mapped back |

A placeholder the model capitalized (`Deployment-1`) is matched as `restore` matches it. A
container that has the owner's name is the same placeholder and so shows the owner's real name.
Node names and private registry hosts often are internal hostnames: whoever needs the Pod or the
node finds them in the Event or in kausa's log.

## What a notification never contains

- Log lines, event messages, Pod status messages or the incident's `sample.message`, as a field.
- The request to the model or any part of its context, and the pseudonym mapping.
- Real names other than the namespace's and the top-level owner's: no container, Pod, node or
  image name in `facts` or in the `Facts:` line.
- In `facts`, any text but a reason of kausa's own list ([Fact
  statements](rules.md#fact-statements)): a reason outside it is `null`.
- Labels, annotations, UIDs, redaction counts, token counts, the prompt version, the model's
  name, the endpoint, the API key and kausa's configuration.
- The webhook's URL or token, which kausa also never logs.

"Never logs" means that no field holds a log line. The explanation is the model's text, which can
repeat the redacted context the model was given (log and event text, probe paths, label keys), up
to the explanation's caps (2,400 characters in all), whether the model is verbose or injected text
asks it to. kausa cannot tell a quote from a summary. Since v0.5.0 the answer passes the pattern
redaction rules before any of it is written, so a value they recognize (an address, a token, a
URL's query, a host outside the allowlist) is redacted even when the model writes it out of the
context or invents it; text they do not recognize, such as a command, is posted as written. The kind test checks that no body carries a log line of the requests, but
its fake model never echoes its input, so it cannot catch a model that does.

## Compatibility

- Within `v2`, fields are only added, never removed, renamed or given another type or meaning,
  and no `type` is added.
- Receivers must ignore the fields they do not know.
- Anything else is `v3`, a new `schema` string, announced in the [Changelog](../../CHANGELOG.md);
  a release sends one version.
- **From `v1`:** `type` is `detected`, `explained` or `capped` instead of always `explained`;
  `facts` is new in `detected` and `explained`; `explanation` is only in `explained`; `capped`
  carries `limit` and none of the incident's fields. A receiver of `v1` that reads `explanation`
  without checking `type` must check it first. The `slack` and `telegram` texts gain the `Facts:`
  line and the mark. kausa sends `v1` up to v0.6.3 and `v2` from the next release on; no release
  sends both.
- To drop duplicates (a send that timed out after the receiver took it, then the retry, or a
  later occurrence of the same incident), key on `fingerprint` and `first_seen`.

## The fingerprint

The fingerprint is FNV-1a, a non-cryptographic 64-bit hash of the rule, the namespace, the
owner's kind and name and a normalized message (or the image, for `D002`, and the container's
name, for `D001` and `D003`, and `revision <n>`, for `D014`), computed before
redaction ([Detection and redaction rules](rules.md)). It was already in the names of kausa's
Events and Explanations; notifications take it out of the cluster. It hides nothing: whoever can
guess the inputs can compute it and confirm a guess. It is an identifier for joining a
notification with the Event and the log, not a secret.

## What Slack and Telegram do with it

ADR 0015 listed what the formats rely on as hypotheses, and they were checked against the official
documentation on 2026-10-28 ([Slack and Telegram for
notifications](../research/2026-10-28-slack-and-telegram-for-notifications.md), which also has the
quotes and the commits of the code that was read). Documentation, not a test: nothing was posted
to Slack or Telegram. Three results changed the design, in the `Update 2026-10-29` of ADR 0015:
the Telegram joiners, the 3-second interval for `telegram`, and `"parse": "none"` for `slack`.

| Hypothesis | Result on 2026-10-28 | Source |
|---|---|---|
| Incoming webhooks honour `mrkdwn: false` | Verified by Slack's general rule for messages; not stated for webhooks by name | [Formatting message text](https://docs.slack.dev/messaging/formatting-message-text/), [Sending messages using incoming webhooks](https://docs.slack.dev/messaging/sending-messages-using-incoming-webhooks/) |
| Incoming webhooks honour `unfurl_links` and `unfurl_media` | Verified, for webhooks by name | [Unfurling links in messages](https://docs.slack.dev/messaging/unfurling-links-in-messages/) |
| Slack escapes `&`, `<` and `>`, and only those | Verified | [Formatting message text](https://docs.slack.dev/messaging/formatting-message-text/) |
| Slack truncates a text at 40,000 characters | Verified | [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/) |
| Plain `@here`, `@channel` or `@everyone` acts in Slack | Refuted for kausa's body by the documentation: only with `link_names`, which kausa does not send. A real post on 2026-10-30 showed the opposite: a plain `@here` and `@channel` notified the channel. kausa puts a word joiner after every `@` of the Slack text since then | [Formatting message text](https://docs.slack.dev/messaging/formatting-message-text/), [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/) |
| A plain URL is a clickable link in Slack | Verified: escaping does not stop it, `"parse": "none"` does. kausa sends `"parse": "none"` since 2026-10-29; that incoming webhooks honour it is not stated by name, but a real post on 2026-10-30 showed the URL as plain text | [Formatting message text](https://docs.slack.dev/messaging/formatting-message-text/), [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/) |
| Slack allows about one message per second | Verified for incoming webhooks; above it, `429` | [Rate limits](https://docs.slack.dev/apis/web-api/rate-limits/) |
| Telegram's field names (`chat_id` as an integer or a string, `text`, `link_preview_options.is_disabled`; `parse_mode` optional) | Verified (`link_preview_options` since Bot API 7.0) | [Bot API](https://core.telegram.org/bots/api), [Bot API changelog](https://core.telegram.org/bots/api-changelog) |
| Telegram's limit is 4,096 characters, and its unit | The limit is verified; the unit is not documented. The Bot API server's code counts code points, and 4,000 UTF-16 units are at most 4,000 code points, so the cut holds either way | [Bot API](https://core.telegram.org/bots/api), [tdlib/td](https://github.com/tdlib/td) |
| A channel's username is 5 to 32 characters | Verified (`A-z`, `0-9` and `_`). kausa also requires a letter first, which the documentation does not say; an integer chat id always works | [account.checkUsername](https://core.telegram.org/method/account.checkUsername) |
| Telegram's HTTP status follows `error_code` | Verified for the open-source Bot API server; that the hosted `api.telegram.org` behaves the same stays a hypothesis | [tdlib/telegram-bot-api](https://github.com/tdlib/telegram-bot-api), [Bot API](https://core.telegram.org/bots/api) |
| Telegram turns @mentions, URLs, hashtags and bot commands in plain text into entities, and a mention notifies the member | Verified: plain text can mention a member of a Telegram group, who is notified even with the group muted, and `sendMessage` has no parameter that turns detection off. kausa therefore puts a word joiner after every `@` and `/` of the Telegram text since 2026-10-29; that this stops the detection is a hypothesis until a real post | [Bot API](https://core.telegram.org/bots/api), [Telegram FAQ](https://telegram.org/faq) |
| Telegram allows about one message per second | Partly refuted: one per second in a chat, but at most 20 messages a minute in a group; above it, `429`. kausa starts `telegram` sends at least 3 seconds apart since 2026-10-29 | [Bots FAQ](https://core.telegram.org/bots/faq) |
| Telegram bot chats are not end-to-end encrypted | Partly verified: cloud chats use server-client encryption; that bots cannot take part in secret chats is not stated | [Telegram FAQ](https://telegram.org/faq) |

What follows for a reader: in Slack, a real post on 2026-10-30 showed a URL as plain text, and the
escaped `<!everyone>` and `<!channel>` as `!everyone` and `!channel`, but a plain `@here` and
`@channel` notified the channel. kausa now puts a word joiner after every `@`, and a second real
post the same day showed `@here`, `@channel` and `@everyone` as plain text ([Security
policy](../../SECURITY.md), "What kausa cannot promise"). In Telegram, a real post on
2026-10-29 showed it: with the joiners, Telegram found no mention and no bot command in the text,
only the URL, which stays a link (without the joiners it found all three).
