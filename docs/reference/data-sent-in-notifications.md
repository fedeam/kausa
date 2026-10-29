# Data sent in notifications

Exactly what kausa posts to the notification webhook for each new explanation, in each of its
three formats, and what a notification never contains.

Status as of 2026-10-29: **released in v0.3.0, off by default**
(`src/notification.rs` renders the bodies, `src/notify.rs` sends them). Why it works this way:
[0015. Post each new explanation to one optional webhook, as JSON, Slack or
Telegram](../adr/0015-notifications.md). The `json` body is checked by unit tests and by the kind
test (`tests/e2e/run.sh`) against a fake receiver; the `slack` and `telegram` bodies are checked
by unit tests only. **Nothing has been posted to a real Slack workspace or Telegram chat yet**: a
receiver is supported only once it has been tested with a real webhook. How to turn it on: [Send
notifications to Slack, Telegram or a webhook](../how-to/send-notifications.md).

## Where it goes

To one URL, the content of the file given with `--notify-url-file` (the chart's
`notifications.existingSecret`), and nowhere else. Notifications are off unless that flag is
given. The model endpoint stays the only destination of the context of an incident ([Data sent
to the provider](data-sent-to-the-provider.md)); the webhook gets only what this page lists.

Every request is a `POST` of a JSON body with:

- `content-type: application/json` and `user-agent: kausa/<version>`, in every format;
- `Authorization: Bearer <token>` only with `--notify-format json` and `--notify-token-file`;
- no cookies, no redirects followed, and no proxy (`HTTP_PROXY` and the like are ignored).

kausa reads only the status code of the answer, never its body.

## When

Once per explanation the model wrote, after its Event: never for an `ExplanationSkipped`
outcome, never in `--dry-run` (a usage error with notifications), and never at start (there is no
test message). A repeated incident explained from the deduplication cache is posted again only
while the webhook has not acknowledged that explanation with a `2xx`, and then with the same
text. So notifications acknowledged with a `2xx` are at most the model calls, which
`--max-calls-per-hour` and `--max-calls-per-day` bound (30 and 200 by default). A receiver that
answers after the 10-second timeout can get duplicates: see [Compatibility](#compatibility).

kausa sends one notification at a time, from an in-memory queue of 16 that drops its oldest when
full, and starts each send at least 1 second after the previous one (3 seconds with `telegram`,
which allows a bot 20 messages a minute in a group). A send that times out, cannot connect, or is
answered `429` or `5xx` is tried once more 30 seconds later; any other failure is final.
Notifications still queued are dropped when kausa stops. The log lines and counters are in
[Command line and log format](command-line.md#notifications).

## The `json` body

`--notify-format json` (the default). An example, with invented values:

```json
{
  "schema": "kausa.fedeam.com/notification/v1",
  "type": "explained",
  "kausa_version": "0.2.0",
  "detection": {"id": "D001", "name": "crash loop"},
  "object": {"apiVersion": "apps/v1", "kind": "Deployment", "namespace": "shop", "name": "web"},
  "fingerprint": "5d0c2b7e9a1f4c38",
  "first_seen": "2026-10-28T11:59:00Z",
  "explanation": {
    "summary": "The container exits right after it starts.",
    "cause": "It is started with a flag it does not know.",
    "steps": ["Check the container's args in the Deployment."],
    "confidence": "medium"
  }
}
```

The unit test `the_json_body_is_the_reference_example` in `src/notification.rs` compares the body
kausa builds with this example, parsed. kausa serializes the keys of each object in alphabetical
order; their order carries no meaning.

| Field | What it holds |
|---|---|
| `schema` | Always `kausa.fedeam.com/notification/v1`. Another version is another string |
| `type` | Always `explained` in v1 |
| `kausa_version` | The version of the kausa binary that sent it |
| `detection.id`, `detection.name` | The rule, from [Detection and redaction rules](rules.md) |
| `object` | The incident's top-level owner, the object of the Event's `regarding`: `apiVersion`, `kind`, and its real `namespace` and `name`. No UID |
| `fingerprint` | The incident fingerprint, 16 hexadecimal digits, the one in kausa's `incident` log line and in the Event's name |
| `first_seen` | When the failure was first seen, RFC 3339 in UTC, the same string as in the `incident` log line |
| `explanation` | The model's `summary`, `cause`, `steps` (zero to five) and `confidence` (`low`, `medium` or `high`), validated and cleaned as for the Event, with only the namespace's and the object's names mapped back ([Names](#names)) |

Nothing else is in the body.

## The text of `slack` and `telegram`

Both formats send one plain-text rendering of the same fields, with no markup (`telegram` adds
invisible joiners to it, below):

```text
kausa: crash loop (D001) in shop, Deployment web
The container exits right after it starts.
Likely cause: It is started with a flag it does not know.
Check:
1. Check the container's args in the Deployment.
Confidence: medium · fingerprint 5d0c2b7e9a1f4c38
```

- The lines are joined with a newline, with none at the end. The `Check:` block is left out when
  the explanation has no steps. The separator in the last line is U+00B7 (middle dot).
- The text is then cut, with no marker, at a character boundary so that it holds at most 4,000
  UTF-16 code units (a character outside the Basic Multilingual Plane, such as most emoji, counts
  two).
- For `slack` only, and after the cut, `&`, `<` and `>` become `&amp;`, `&lt;` and `&gt;`, and
  nothing else is escaped. The Slack text is therefore at most 20,000 units, and an entity is
  never cut in two.
- For `telegram` only, U+2060 (WORD JOINER, invisible, one UTF-16 unit) follows every `@` and
  every `/`, so that the text looks the same and Telegram should find no `@username` mention,
  `/command` or `/command@bot` in it (a hypothesis until the first post to a real chat: see [What
  Slack and Telegram do with it](#what-slack-and-telegram-do-with-it)). The joiners are added
  after the cut, and the cut counts them, so the Telegram text, joiners included, is at most
  4,000 units: with many `@` or `/` it is cut earlier than the Slack text. Taking the joiners out
  gives a prefix of the Slack text before escaping.

## The three bodies

| Format | Body |
|---|---|
| `json` | The [`json` body](#the-json-body) above |
| `slack` | `{"text": <the escaped text>, "mrkdwn": false, "unfurl_links": false, "unfurl_media": false, "parse": "none"}` |
| `telegram` | `{"chat_id": <the chat>, "text": <the text with its joiners>, "link_preview_options": {"is_disabled": true}}`, with no `parse_mode`. `chat_id` is a JSON number for an integer chat id (`-1001234567890`) and a string for a channel's username (`"@kausa_alerts"`), from `--notify-telegram-chat-id` |

`mrkdwn: false` turns Slack's formatting off, `unfurl_links` and `unfurl_media` its previews, and
`"parse": "none"` its turning of a plain URL into a link (Slack does not say that incoming
webhooks honour `parse`; one that ignores it is no worse off). The unit tests
`the_slack_body_is_the_reference_text_with_formatting_links_and_previews_off` and
`the_telegram_body_is_plain_text_with_previews_off` compare both bodies of the example.

The `telegram` body of the example, with the chat id `-1001234567890` (its text has no `@` or
`/`, so it carries no joiner):

```json
{
  "chat_id": -1001234567890,
  "text": "kausa: crash loop (D001) in shop, Deployment web\nThe container exits right after it starts.\nLikely cause: It is started with a flag it does not know.\nCheck:\n1. Check the container's args in the Deployment.\nConfidence: medium · fingerprint 5d0c2b7e9a1f4c38",
  "link_preview_options": {"is_disabled": true}
}
```

The `slack` body of the example is the same text under `text` (it has no `&`, `<` or `>`), with
`"mrkdwn": false`, `"unfurl_links": false`, `"unfurl_media": false` and `"parse": "none"`.

## Names

The Event and kausa's log map every placeholder of the explanation back to the real name. A
notification maps back only two, through `Redactor::restore_only`:

| Name | In a notification |
|---|---|
| The incident's namespace (`namespace-1`) | The real name |
| The top-level owner (`deployment-1`, `job-1`, …; `pod-1` when the owner is a bare Pod) | The real name |
| Pods, containers, nodes, service accounts (`pod-1`, `container-1`, `node-1`, `serviceaccount-1`) | Stay placeholders |
| Images from private registries (`registry-1/image-1:tag`) | Stay placeholders |
| Images from public registries and Docker Hub | As the model wrote them: they were never placeholders |
| Values that redaction removed (`<redacted:ip>`, …) | Stay redacted |

A placeholder the model capitalized (`Deployment-1`) is matched as `restore` matches it. A
container that has the owner's name is the same placeholder and so shows the owner's real name.
Node names and private registry hosts often are internal hostnames: whoever needs the Pod or the
node finds them in the Event or in kausa's log.

## What a notification never contains

- Log lines, event messages, Pod status messages or the incident's `sample.message`, as a field.
- The request to the model or any part of its context, and the pseudonym mapping.
- Real names other than the namespace's and the top-level owner's.
- Labels, annotations, UIDs, redaction counts, token counts, the prompt version, the model's
  name, the endpoint, the API key and kausa's configuration.
- The webhook's URL or token, which kausa also never logs.

"Never logs" means that no field holds a log line. The explanation is the model's text, which can
repeat the redacted context the model was given (log and event text, probe paths, label keys), up
to the explanation's caps (2,400 characters in all), whether the model is verbose or injected text
asks it to. kausa cannot tell a quote from a summary. The kind test checks that no body carries a log line of the requests, but
its fake model never echoes its input, so it cannot catch a model that does.

## Compatibility

- Within `v1`, fields are only added, never removed, renamed or given another type or meaning.
- Receivers must ignore the fields they do not know.
- Anything else is `v2`, a new `schema` string, announced in the [Changelog](../../CHANGELOG.md);
  a release sends one version.
- To drop duplicates (a send that timed out after the receiver took it, then the retry, or a
  later occurrence of the same incident), key on `fingerprint` and `first_seen`.

## The fingerprint

The fingerprint is FNV-1a, a non-cryptographic 64-bit hash of the rule, the namespace, the
owner's kind and name and a normalized message (or the image, for `D002`, and the container's
name, for `D001` and `D003`), computed before
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
| Plain `@here`, `@channel` or `@everyone` acts in Slack | Refuted for kausa's body: only with `link_names`, which kausa does not send | [Formatting message text](https://docs.slack.dev/messaging/formatting-message-text/), [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/) |
| A plain URL is a clickable link in Slack | Verified: escaping does not stop it, `"parse": "none"` does. kausa sends `"parse": "none"` since 2026-10-29; that incoming webhooks honour it is not stated by name | [Formatting message text](https://docs.slack.dev/messaging/formatting-message-text/), [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/) |
| Slack allows about one message per second | Verified for incoming webhooks; above it, `429` | [Rate limits](https://docs.slack.dev/apis/web-api/rate-limits/) |
| Telegram's field names (`chat_id` as an integer or a string, `text`, `link_preview_options.is_disabled`; `parse_mode` optional) | Verified (`link_preview_options` since Bot API 7.0) | [Bot API](https://core.telegram.org/bots/api), [Bot API changelog](https://core.telegram.org/bots/api-changelog) |
| Telegram's limit is 4,096 characters, and its unit | The limit is verified; the unit is not documented. The Bot API server's code counts code points, and 4,000 UTF-16 units are at most 4,000 code points, so the cut holds either way | [Bot API](https://core.telegram.org/bots/api), [tdlib/td](https://github.com/tdlib/td) |
| A channel's username is 5 to 32 characters | Verified (`A-z`, `0-9` and `_`). kausa also requires a letter first, which the documentation does not say; an integer chat id always works | [account.checkUsername](https://core.telegram.org/method/account.checkUsername) |
| Telegram's HTTP status follows `error_code` | Verified for the open-source Bot API server; that the hosted `api.telegram.org` behaves the same stays a hypothesis | [tdlib/telegram-bot-api](https://github.com/tdlib/telegram-bot-api), [Bot API](https://core.telegram.org/bots/api) |
| Telegram turns @mentions, URLs, hashtags and bot commands in plain text into entities, and a mention notifies the member | Verified: plain text can mention a member of a Telegram group, who is notified even with the group muted, and `sendMessage` has no parameter that turns detection off. kausa therefore puts a word joiner after every `@` and `/` of the Telegram text since 2026-10-29; that this stops the detection is a hypothesis until a real post | [Bot API](https://core.telegram.org/bots/api), [Telegram FAQ](https://telegram.org/faq) |
| Telegram allows about one message per second | Partly refuted: one per second in a chat, but at most 20 messages a minute in a group; above it, `429`. kausa starts `telegram` sends at least 3 seconds apart since 2026-10-29 | [Bots FAQ](https://core.telegram.org/bots/faq) |
| Telegram bot chats are not end-to-end encrypted | Partly verified: cloud chats use server-client encryption; that bots cannot take part in secret chats is not stated | [Telegram FAQ](https://telegram.org/faq) |

What follows for a reader, as of 2026-10-29, from the documentation only: in Slack, the text of a
notification should mention nobody, and should show a URL as plain text if the webhook honours
`parse`; this stays listed under "What kausa cannot promise" in the [Security
policy](../../SECURITY.md) until a post to a real workspace shows it. In Telegram, a real post on
2026-10-29 showed it: with the joiners, Telegram found no mention and no bot command in the text,
only the URL, which stays a link (without the joiners it found all three).
