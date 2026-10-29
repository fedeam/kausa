# Slack and Telegram for notifications

What Slack's and Telegram's official documentation say about the hypotheses of [0015. Post each
new explanation to one optional webhook, as JSON, Slack or
Telegram](../adr/0015-notifications.md), checked on 2026-10-28 before the body builders are
written. Every page below was read on that date; where the documentation is silent, the official
open-source code of the Telegram Bot API server and of TDLib was read instead, at the commits
given. Nothing was posted to a real Slack workspace or Telegram chat: the ADR lists each receiver
as supported only after the maintainer has posted to it.

## Result

| Hypothesis in 0015 | Result | Source |
|---|---|---|
| Incoming webhooks honour `mrkdwn: false` | Verified by the general rule: webhooks "conform to the same rules and functionality as any of our other messaging APIs", and the top-level `text` turns formatting off with `"mrkdwn": false`. Not stated for webhooks by name | [S1], [S2] |
| Incoming webhooks honour `unfurl_links` and `unfurl_media` | Verified, for webhooks by name: "This applies to messages posted via incoming webhooks"; both set to `false` stop all unfurls | [S3] |
| Slack asks to escape `&`, `<` and `>`, and only those | Verified: "Slack uses &, <, and > as control characters", and "only the specific characters shown above will be decoded" | [S2] |
| Slack truncates a message's text at 40,000 characters | Verified: "Slack will truncate messages containing more than 40,000 characters"; it recommends at most 4,000 | [S4] |
| A plain `@here`, `@channel` or `@everyone` acts | Refuted for kausa's body: automatic parsing of the top-level `text` happens only with `link_names` set to `1`, which kausa does not send, and `link_names` "no longer supports linking individual users" | [S2], [S4] |
| A plain URL becomes a link in Slack | Verified: "Regular URLs will still be converted into clickable links"; `"parse": "none"` turns that off. Escaping does not | [S2], [S4] |
| Slack allows about one message per second | Verified: incoming webhooks, "1 per second", short bursts allowed; above it, `429` with `Retry-After` | [S5] |
| Telegram field names: `chat_id`, `text`, `link_preview_options.is_disabled`, no `parse_mode` | Verified: `chat_id` is "Integer or String", `link_preview_options` is a `LinkPreviewOptions` whose `is_disabled` is "True, if the link preview is disabled", and `parse_mode` is optional. `link_preview_options` replaced `disable_web_page_preview` in Bot API 7.0, December 29, 2023 | [T1], [T2] |
| Telegram's limit is 4,096, and its unit | The limit is verified: "1-4096 characters after entities parsing". The unit is not in the documentation; TDLib, which the Bot API server is built on, counts a bot's text in Unicode code points (`utf8_length`). kausa's 4,000 UTF-16 units are at most 4,000 code points, so the cut holds either way | [T1], [C2] |
| A channel's username is `@` and 5 to 32 characters | Verified: "Accepted characters: A-z (case-insensitive), 0-9 and underscores. Length: 5-32 characters." The documentation does not say that the first character must be a letter | [T3], [T4] |
| Telegram's HTTP status follows `error_code` | Verified for the official open-source Bot API server: an error's HTTP status and its `error_code` are the same value, and a success is always `200` with `"ok": true`. The documentation says only that `error_code` "is also returned, but its contents are subject to change"; that the hosted `api.telegram.org` behaves like the open-source server stays a hypothesis | [T1], [C1] |
| Telegram turns @mentions, URLs, hashtags and commands in plain text into entities | Verified in substance: a text message's `entities` hold "special entities like usernames, URLs, bot commands, etc. that appear in the text", with the types `mention`, `url`, `bot_command`, `hashtag`, `email` and `phone_number`; automatic detection is stated in so many words for rich messages. `sendMessage` has no parameter that turns detection off (`skip_entity_detection` exists only for rich messages) | [T1] |
| A mention in a group notifies that member | Verified: a member who muted a group still gets "notifications only when people mention you or reply to your messages" | [T5] |
| Telegram allows about one message per second | Partly refuted: "In a single chat, avoid sending more than one message per second", but "in a group, bots are not be able to send more than 20 messages per minute". Above the limits, `429` | [T6] |
| Telegram bot chats are not end-to-end encrypted | Partly verified: cloud chats, private and group, use server-client encryption, and only secret chats are end to end. That bots cannot take part in secret chats is not stated | [T7] |

## What it means for 0015

- **Telegram: plain text can mention a member.** The model's text, which a tenant can shape, can
  carry `@username`; Telegram turns it into a mention, and the member is notified even in a muted
  group. The text can also carry `/command@OtherBot`, which, in a group where that bot has
  Bot-to-Bot Communication Mode on, the bot receives and can answer [T8]. 0015 says that if this
  check finds that plain text can mention a member, the ADR gets an `Update` with the fix, decided
  then. That decision belongs to the maintainer, before the Telegram body is written. The options
  seen here, none tested: accept and document it under "What kausa cannot promise"; break `@` and
  `/` in the text with an invisible character (U+2060, word joiner), which changes the text of
  both formats; or send an explicit empty `entities` list, whose effect on detection is not
  documented.
- **Slack: plain text does not mention anyone, but a plain URL is a link.** The escaping and the
  absence of `link_names` keep `@here`, `@channel` and `@everyone` inert. A URL in the text is
  still clickable unless the body adds `"parse": "none"`; 0015 already lists clickable links
  under what kausa cannot promise. Whether a webhook honours `parse` is not stated for webhooks
  by name.
- **Telegram groups: 20 messages a minute.** kausa's one second between sends keeps it under the
  per-chat rate, but a burst of more than 20 explanations within a minute to a group would get
  `429`s, each retried once after 30 s. The default budget (30 calls an hour) makes this rare. A
  wider interval for `telegram` (3 s) or the current one with the documented risk is a transport
  decision, not a body change.
- **Unchanged:** the three escapes, `mrkdwn`, `unfurl_*`, the 4,000- and 20,000-unit caps, the
  Telegram field names, reading only the status code, and Slack's rate all hold as 0015 wrote
  them. The chat id rule `[A-Za-z][A-Za-z0-9_]{4,31}` is stricter than the documented one
  (`[A-Za-z0-9_]{5,32}`) on the first character; a channel can always be named by its integer id.

## Sources

All read on 2026-10-28.

- [S1] Slack, [Sending messages using incoming
  webhooks](https://docs.slack.dev/messaging/sending-messages-using-incoming-webhooks/).
- [S2] Slack, [Formatting message text](https://docs.slack.dev/messaging/formatting-message-text/):
  Escaping text, Special mentions, Automatic parsing, Disabling formatting.
- [S3] Slack, [Unfurling links in
  messages](https://docs.slack.dev/messaging/unfurling-links-in-messages/): classic unfurling.
- [S4] Slack, [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/):
  arguments `link_names`, `mrkdwn`, `parse`, `unfurl_links`, `unfurl_media`; Formatting messages;
  Truncating content.
- [S5] Slack, [Rate limits](https://docs.slack.dev/apis/web-api/rate-limits/).
- [T1] Telegram, [Bot API](https://core.telegram.org/bots/api) (Bot API 10.3): Making requests,
  `sendMessage`, `LinkPreviewOptions`, `MessageEntity`, `Message.entities`, rich messages.
- [T2] Telegram, [Bot API changelog](https://core.telegram.org/bots/api-changelog): Bot API 7.0.
- [T3] Telegram, [account.checkUsername](https://core.telegram.org/method/account.checkUsername).
- [T4] Telegram, [Bot features](https://core.telegram.org/bots/features): usernames.
- [T5] Telegram, [FAQ](https://telegram.org/faq): smart notifications.
- [T6] Telegram, [Bots FAQ](https://core.telegram.org/bots/faq): "My bot is hitting limits, how do
  I avoid this?".
- [T7] Telegram, [FAQ](https://telegram.org/faq): encryption, secret chats.
- [T8] Telegram, [Bot features](https://core.telegram.org/bots/features): Bot-to-Bot
  Communication.
- [C1] [tdlib/telegram-bot-api](https://github.com/tdlib/telegram-bot-api) at commit `e3e9dd8`:
  `fail_query` in `telegram-bot-api/Query.h` writes the same code as the HTTP status and as
  `error_code`, and `Query::set_ok` in `Query.cpp` answers `200`.
- [C2] [tdlib/td](https://github.com/tdlib/td) at commit `4c6377a`: the bot text check in
  `td/telegram/MessageContent.cpp` (`utf8_length` against `message_text_length_max`), and
  `utf8_length` in `tdutils/td/utils/utf8.h`, which counts code points.

## Limits

- Documentation, not a test. Nothing was posted to Slack or Telegram; a real post can still differ,
  and Slack-compatible receivers (Mattermost, Discord's `/slack` path) were not checked.
- The hosted Bot API may not run the open-source server's code as published; the limits are the
  ones documented today, and Telegram changes its Bot API often (10.3 on August 24, 2026).
