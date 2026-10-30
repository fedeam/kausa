//! The bodies of a notification (docs/adr/0015-notifications.md): the `json`, `slack` and
//! `telegram` bodies of one new explanation, built from plain strings. Pure: no I/O and no
//! `crate::` import, so that the `notification` fuzz target includes it by path. A tenant shapes
//! its input through the model, and the release profile aborts on a panic, so nothing here may
//! panic.

use serde_json::{Value, json};

/// The `schema` of every `json` body; within `v1`, fields are only added.
pub const SCHEMA: &str = "kausa.fedeam.com/notification/v1";
/// The cap of the plain text, in UTF-16 code units, Telegram's joiners included: under Telegram's
/// 4,096 characters whichever way they are counted.
pub const MAX_TEXT_UNITS: usize = 4000;
/// The cap of the Slack text after the escaping: each unit of the cut text becomes at most five
/// (`&amp;`), well under the 40,000 characters at which Slack truncates a message.
pub const MAX_SLACK_UNITS: usize = 5 * MAX_TEXT_UNITS;
// Checked when it compiles: the escaped text stays under the length at which Slack truncates.
const _: () = assert!(MAX_SLACK_UNITS < 40_000);
/// U+2060, WORD JOINER, which Telegram's text carries after every `@` and `/`, and Slack's after
/// every `@`: invisible, and one UTF-16 code unit.
pub const JOINER: char = '\u{2060}';

/// What a notification says about one new explanation. The names are the real ones of the
/// namespace and the top-level owner; the explanation's texts went through the narrow restore,
/// so every other name in them is still a placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub detection_id: String,
    pub detection_name: String,
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub fingerprint: String,
    /// RFC 3339, UTC, as in the incident's log line.
    pub first_seen: String,
    pub summary: String,
    pub cause: String,
    pub steps: Vec<String>,
    pub confidence: String,
}

/// Telegram's `chat_id`: an integer, sent as a number, or a channel's username, kept with its
/// `@` and sent as a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatId {
    Id(i64),
    Username(String),
}

/// A chat id: an integer that fits in an `i64` and is not zero (decimal digits, with an optional
/// leading `-` and no `+`), or `@` and a username of 5 to 32 ASCII letters, digits and
/// underscores that starts with a letter. Anything else is `None`.
pub fn parse_chat_id(text: &str) -> Option<ChatId> {
    if let Some(username) = text.strip_prefix('@') {
        let starts_with_letter = username
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic());
        let allowed = username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        // All ASCII, so the length in bytes is the length in characters.
        return (starts_with_letter && allowed && (5..=32).contains(&username.len()))
            .then(|| ChatId::Username(text.to_owned()));
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match text.parse::<i64>() {
        Ok(id) if id != 0 => Some(ChatId::Id(id)),
        _ => None,
    }
}

/// The `json` body: these fields and nothing else.
pub fn json_body(n: &Notice, kausa_version: &str) -> Value {
    json!({
        "schema": SCHEMA,
        "type": "explained",
        "kausa_version": kausa_version,
        "detection": {"id": n.detection_id, "name": n.detection_name},
        "object": {
            "apiVersion": n.api_version,
            "kind": n.kind,
            "namespace": n.namespace,
            "name": n.name,
        },
        "fingerprint": n.fingerprint,
        "first_seen": n.first_seen,
        "explanation": {
            "summary": n.summary,
            "cause": n.cause,
            "steps": n.steps,
            "confidence": n.confidence,
        },
    })
}

/// The plain text that `slack` and `telegram` share, cut on a character boundary to at most
/// `MAX_TEXT_UNITS` UTF-16 code units, with no marker: what the tests compare each one with once
/// its joiners ([`telegram_text`], [`slack_text`]) are taken out.
#[cfg(test)]
pub fn text(n: &Notice) -> String {
    cut(render(n), char::len_utf16)
}

/// The text Telegram gets: the rendering cut so that it stays within `MAX_TEXT_UNITS` UTF-16 code
/// units with its joiners, then a `JOINER` after every `@` and every `/`. It looks the same, and
/// the joiners are meant to keep Telegram from finding a mention, bot command or `/command@bot` in
/// it (seen in a real post on 2026-10-29; a URL stays a link; ADR 0015). A prefix of [`text`] once
/// each joiner after an `@` or a `/` is taken out.
pub fn telegram_text(n: &Notice) -> String {
    joined(n, |c| matches!(c, '@' | '/'))
}

/// The text Slack gets before its escaping: the rendering cut so that it stays within
/// `MAX_TEXT_UNITS` UTF-16 code units with its joiners, then a `JOINER` after every `@`. A real
/// post on 2026-10-30 showed that `"parse": "none"` does not keep Slack from turning a plain
/// `@here` or `@channel` into a mention that notifies the channel; the joiner is meant to (ADR
/// 0015). A prefix of [`text`] once each joiner after an `@` is taken out.
pub fn slack_text(n: &Notice) -> String {
    joined(n, |c| c == '@')
}

/// The rendering cut so that it stays within `MAX_TEXT_UNITS` UTF-16 code units once a `JOINER`
/// follows every character `is_joined` picks, with those joiners.
fn joined(n: &Notice, is_joined: impl Fn(char) -> bool) -> String {
    let cut = cut(render(n), |c| c.len_utf16() + usize::from(is_joined(c)));
    let mut joined = String::with_capacity(cut.len() + 3 * cut.matches(&is_joined).count());
    for c in cut.chars() {
        joined.push(c);
        if is_joined(c) {
            joined.push(JOINER);
        }
    }
    joined
}

/// The whole text, before any cut.
fn render(n: &Notice) -> String {
    let mut lines = Vec::with_capacity(5 + n.steps.len());
    lines.push(format!(
        "kausa: {} ({}) in {}, {} {}",
        n.detection_name, n.detection_id, n.namespace, n.kind, n.name
    ));
    lines.push(n.summary.clone());
    lines.push(format!("Likely cause: {}", n.cause));
    if !n.steps.is_empty() {
        lines.push("Check:".to_owned());
        for (i, step) in n.steps.iter().enumerate() {
            lines.push(format!("{}. {step}", i + 1));
        }
    }
    lines.push(format!(
        "Confidence: {} \u{b7} fingerprint {}",
        n.confidence, n.fingerprint
    ));
    lines.join("\n")
}

/// The longest prefix of `text`, on a character boundary, whose characters cost at most
/// `MAX_TEXT_UNITS` in all, each at `cost`.
fn cut(mut text: String, cost: impl Fn(char) -> usize) -> String {
    let mut units: usize = 0;
    let end = text.char_indices().find_map(|(i, c)| {
        units = units.saturating_add(cost(c));
        (units > MAX_TEXT_UNITS).then_some(i)
    });
    if let Some(end) = end {
        // `end` comes from `char_indices`, so it is a character boundary.
        text.truncate(end);
    }
    text
}

/// The `slack` body: the text with its joiners, cut first and then escaped, with formatting,
/// previews and the turning of plain URLs into links (`"parse": "none"`) off.
pub fn slack_body(n: &Notice) -> Value {
    json!({
        "text": escape(&slack_text(n)),
        "mrkdwn": false,
        "unfurl_links": false,
        "unfurl_media": false,
        "parse": "none",
    })
}

/// The `telegram` body: the text with its joiners, with no `parse_mode` and previews off.
pub fn telegram_body(n: &Notice, chat: &ChatId) -> Value {
    let chat_id = match chat {
        ChatId::Id(id) => Value::from(*id),
        ChatId::Username(username) => Value::from(username.as_str()),
    };
    json!({
        "chat_id": chat_id,
        "text": telegram_text(n),
        "link_preview_options": {"is_disabled": true},
    })
}

/// Slack's three control characters, and only those, as entities. No text then becomes a
/// mention or a link, and kausa's `<redacted:…>` markers are shown as they are.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// The example of docs/adr/0015-notifications.md, copied verbatim into the reference page
    /// docs/reference/data-sent-in-notifications.md.
    const GOLDEN_JSON: &str = r#"{
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
}"#;

    /// The text of the same example (the ADR's six lines), with no trailing newline.
    const GOLDEN_TEXT: &str = "kausa: crash loop (D001) in shop, Deployment web\n\
        The container exits right after it starts.\n\
        Likely cause: It is started with a flag it does not know.\n\
        Check:\n\
        1. Check the container's args in the Deployment.\n\
        Confidence: medium \u{b7} fingerprint 5d0c2b7e9a1f4c38";

    fn notice() -> Notice {
        Notice {
            detection_id: "D001".into(),
            detection_name: "crash loop".into(),
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            namespace: "shop".into(),
            name: "web".into(),
            fingerprint: "5d0c2b7e9a1f4c38".into(),
            first_seen: "2026-10-28T11:59:00Z".into(),
            summary: "The container exits right after it starts.".into(),
            cause: "It is started with a flag it does not know.".into(),
            steps: vec!["Check the container's args in the Deployment.".into()],
            confidence: "medium".into(),
        }
    }

    /// The notice with every text field, the names included, set to `text`.
    fn filled(text: &str, steps: usize) -> Notice {
        Notice {
            detection_id: text.into(),
            detection_name: text.into(),
            api_version: text.into(),
            kind: text.into(),
            namespace: text.into(),
            name: text.into(),
            fingerprint: text.into(),
            first_seen: text.into(),
            summary: text.into(),
            cause: text.into(),
            steps: vec![text.to_owned(); steps],
            confidence: text.into(),
        }
    }

    /// The rendering of the contract before the cut, written out independently.
    fn uncut(n: &Notice) -> String {
        let mut lines = vec![
            format!(
                "kausa: {} ({}) in {}, {} {}",
                n.detection_name, n.detection_id, n.namespace, n.kind, n.name
            ),
            n.summary.clone(),
            format!("Likely cause: {}", n.cause),
        ];
        if !n.steps.is_empty() {
            lines.push("Check:".into());
            for (i, step) in n.steps.iter().enumerate() {
                lines.push(format!("{}. {step}", i + 1));
            }
        }
        lines.push(format!(
            "Confidence: {} \u{b7} fingerprint {}",
            n.confidence, n.fingerprint
        ));
        lines.join("\n")
    }

    fn units(text: &str) -> usize {
        text.encode_utf16().count()
    }

    fn unescape(text: &str) -> String {
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    /// Every `&` starts one of the three entities.
    fn whole_entities(text: &str) -> bool {
        text.match_indices('&').all(|(i, _)| {
            let rest = &text[i..];
            rest.starts_with("&amp;") || rest.starts_with("&lt;") || rest.starts_with("&gt;")
        })
    }

    /// The text without the one joiner after each of `joined`; a joiner anywhere else stays.
    fn unjoin(text: &str, joined: &[char]) -> String {
        let mut plain = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            plain.push(c);
            if joined.contains(&c) {
                assert_eq!(
                    chars.next(),
                    Some(JOINER),
                    "no joiner after {c:?} in {text:?}"
                );
            }
        }
        plain
    }

    fn telegram_of(n: &Notice) -> String {
        telegram_body(n, &ChatId::Id(-1))["text"]
            .as_str()
            .expect("the Telegram text is a string")
            .to_owned()
    }

    fn slack_of(n: &Notice) -> String {
        slack_body(n)["text"]
            .as_str()
            .expect("the Slack text is a string")
            .to_owned()
    }

    /// Checks every invariant of the three bodies for `n`.
    fn check_bodies(n: &Notice, case: &str) {
        let t = text(n);
        assert!(units(&t) <= MAX_TEXT_UNITS, "{case}: {} units", units(&t));
        assert!(
            uncut(n).starts_with(&t),
            "{case}: not a prefix of the rendering"
        );
        let slack = slack_of(n);
        assert!(
            units(&slack) <= MAX_SLACK_UNITS,
            "{case}: {}",
            units(&slack)
        );
        assert!(!slack.contains('<') && !slack.contains('>'), "{case}");
        assert!(whole_entities(&slack), "{case}: a split entity");
        // Every '@' carries a joiner, and without them the unescaped text is a prefix of the cut
        // text, or the cut text itself when it has none.
        let plain = unjoin(&unescape(&slack), &['@']);
        assert!(
            t.starts_with(&plain),
            "{case}: escaped before the cut, or not a prefix of the cut text"
        );
        if !t.contains('@') {
            assert_eq!(plain, t, "{case}");
        }
        let telegram = telegram_of(n);
        assert!(
            units(&telegram) <= MAX_TEXT_UNITS,
            "{case}: {} units in Telegram's text",
            units(&telegram)
        );
        // Every '@' and '/' carries a joiner, and without them the text is a prefix of the cut
        // text, or the cut text itself when it has neither.
        let plain = unjoin(&telegram, &['@', '/']);
        assert!(
            t.starts_with(&plain),
            "{case}: not a prefix of the cut text"
        );
        if !t.contains(['@', '/']) {
            assert_eq!(plain, t, "{case}");
        }
        let json = json_body(n, "0.2.0");
        let parsed: Value = serde_json::from_str(&json.to_string()).expect("the json body parses");
        assert_eq!(parsed, json, "{case}");
    }

    #[test]
    fn the_constants_of_the_contract() {
        assert_eq!(SCHEMA, "kausa.fedeam.com/notification/v1");
        assert_eq!(MAX_TEXT_UNITS, 4000);
        assert_eq!(MAX_SLACK_UNITS, 20_000);
    }

    #[test]
    fn the_json_body_is_the_reference_example() {
        let golden: Value = serde_json::from_str(GOLDEN_JSON).expect("the golden JSON parses");
        assert_eq!(json_body(&notice(), "0.2.0"), golden);
        // The version is the one given, and nothing else is added.
        let body = json_body(&notice(), "9.8.7");
        assert_eq!(body["kausa_version"], "9.8.7");
        let keys: Vec<&str> = body
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "detection",
                "explanation",
                "fingerprint",
                "first_seen",
                "kausa_version",
                "object",
                "schema",
                "type"
            ]
        );
    }

    #[test]
    fn the_text_is_the_reference_rendering() {
        assert_eq!(text(&notice()), GOLDEN_TEXT);
        assert_eq!(uncut(&notice()), GOLDEN_TEXT);
        assert!(!text(&notice()).ends_with('\n'));
    }

    #[test]
    fn the_slack_body_is_the_reference_text_with_formatting_links_and_previews_off() {
        assert_eq!(
            slack_body(&notice()),
            json!({
                "text": GOLDEN_TEXT,
                "mrkdwn": false,
                "unfurl_links": false,
                "unfurl_media": false,
                "parse": "none",
            })
        );
    }

    #[test]
    fn the_telegram_body_is_plain_text_with_previews_off() {
        let chat = ChatId::Id(-1001234567890);
        let body = telegram_body(&notice(), &chat);
        assert_eq!(
            body,
            json!({
                "chat_id": -1001234567890_i64,
                "text": GOLDEN_TEXT,
                "link_preview_options": {"is_disabled": true},
            })
        );
        assert!(body.get("parse_mode").is_none());
        // A channel's username is sent as a string, with its '@'.
        let chat = parse_chat_id("@kausa_alerts").expect("a valid username");
        let body = telegram_body(&notice(), &chat);
        assert_eq!(body["chat_id"], "@kausa_alerts");
        assert!(body.get("parse_mode").is_none());
        assert_eq!(body["link_preview_options"], json!({"is_disabled": true}));
    }

    #[test]
    fn without_steps_the_check_block_is_left_out() {
        let mut n = notice();
        n.steps.clear();
        assert_eq!(
            text(&n),
            "kausa: crash loop (D001) in shop, Deployment web\n\
             The container exits right after it starts.\n\
             Likely cause: It is started with a flag it does not know.\n\
             Confidence: medium \u{b7} fingerprint 5d0c2b7e9a1f4c38"
        );
        assert_eq!(json_body(&n, "0.2.0")["explanation"]["steps"], json!([]));
    }

    #[test]
    fn steps_are_numbered_from_one() {
        let mut n = notice();
        n.steps = (1..=5).map(|i| format!("step {i}")).collect();
        let t = text(&n);
        assert!(
            t.contains(
                "\nCheck:\n1. step 1\n2. step 2\n3. step 3\n4. step 4\n5. step 5\nConfidence: "
            ),
            "{t}"
        );
    }

    #[test]
    fn slack_escapes_ampersands_and_angle_brackets_in_every_field() {
        let hostile = "a&b <redacted:ip> <!channel> <https://evil.example|click> x>y";
        let n = filled(hostile, 2);
        let slack = slack_of(&n);
        assert!(!slack.contains('<') && !slack.contains('>'), "{slack}");
        assert!(slack.contains("&lt;redacted:ip&gt;"), "{slack}");
        assert!(slack.contains("&lt;!channel&gt;"), "{slack}");
        assert!(slack.contains("a&amp;b"), "{slack}");
        assert!(whole_entities(&slack), "{slack}");
        assert_eq!(unescape(&slack), text(&n));
        // Every field of the text, the names included (header 5, summary, cause, two steps,
        // confidence and fingerprint), went through the escaping.
        assert_eq!(slack.matches("&lt;redacted:ip&gt;").count(), 11, "{slack}");
        // Only those three characters: quotes stay.
        let mut quoted = notice();
        quoted.summary = r#"it's "quoted""#.into();
        assert!(slack_of(&quoted).contains(r#"it's "quoted""#));
        // The other bodies keep the text as it is, but for Telegram's joiners.
        assert!(text(&n).contains("<redacted:ip>"));
        assert_eq!(
            telegram_body(&n, &ChatId::Id(1))["text"],
            text(&n).replace('/', "/\u{2060}")
        );
        assert_eq!(json_body(&n, "0.2.0")["explanation"]["summary"], hostile);
        assert_eq!(json_body(&n, "0.2.0")["object"]["name"], hostile);
        check_bodies(&n, "hostile");
    }

    #[test]
    fn the_text_is_cut_to_4000_units_on_a_character_boundary() {
        // Every field at its cap (300, 600 and five steps of 300 characters, a namespace of 63
        // and a name of 253) with characters outside the Basic Multilingual Plane, two units
        // each. A header of odd length makes the cut fall inside a surrogate pair.
        let emoji = "\u{1F600}";
        let mut n = filled(emoji, 0);
        n.detection_id = "D001".into();
        n.detection_name = "crash loop".into();
        n.namespace = "a".repeat(63);
        n.name = emoji.repeat(253);
        n.summary = emoji.repeat(300);
        n.cause = emoji.repeat(600);
        n.steps = vec![emoji.repeat(300); 5];
        n.confidence = "low".into();
        let full = uncut(&n);
        assert!(units(&full) > MAX_TEXT_UNITS);
        let t = text(&n);
        assert!(units(&t) <= MAX_TEXT_UNITS, "{}", units(&t));
        assert!(
            units(&t) >= MAX_TEXT_UNITS - 1,
            "cut too early: {}",
            units(&t)
        );
        // A prefix of the rendering, with no marker added.
        assert!(full.starts_with(&t));
        check_bodies(&n, "emoji at the caps");
        // The json body is not cut.
        assert_eq!(json_body(&n, "0.2.0")["explanation"]["summary"], n.summary);
        assert_eq!(json_body(&n, "0.2.0")["object"]["name"], n.name);
    }

    #[test]
    fn an_ascii_text_over_the_cap_keeps_exactly_4000_units() {
        let mut n = notice();
        n.summary = "x".repeat(5000);
        assert_eq!(units(&text(&n)), MAX_TEXT_UNITS);
        assert!(uncut(&n).starts_with(&text(&n)));
    }

    #[test]
    fn a_text_of_ampersands_at_the_cap_keeps_whole_entities_within_20000_units() {
        // Every field at its cap, all '&'.
        let mut n = filled("&", 0);
        n.namespace = "&".repeat(63);
        n.name = "&".repeat(253);
        n.summary = "&".repeat(300);
        n.cause = "&".repeat(600);
        n.steps = vec!["&".repeat(300); 5];
        check_bodies(&n, "ampersands at the field caps");
        // Over the text cap: cut to 4000 units first, then escaped, five units each.
        let mut n = notice();
        n.summary = "&".repeat(5000);
        let t = text(&n);
        assert_eq!(units(&t), MAX_TEXT_UNITS);
        let slack = slack_of(&n);
        let header = "kausa: crash loop (D001) in shop, Deployment web\n";
        assert_eq!(
            units(&slack),
            header.len() + 5 * (MAX_TEXT_UNITS - header.len())
        );
        check_bodies(&n, "ampersands over the cap");
        // The worst case: every unit of the cut text is one '&' or '<'.
        for c in ["&", "<", ">"] {
            let n = filled(&c.repeat(MAX_TEXT_UNITS), 5);
            check_bodies(&n, c);
            assert!(units(&slack_of(&n)) <= MAX_SLACK_UNITS);
        }
    }

    #[test]
    fn telegram_gets_a_word_joiner_after_every_at_sign_and_slash() {
        for (cause, joined) in [
            ("Ask @someone.", "Ask @\u{2060}someone."),
            (
                "It answers /start@OtherBot.",
                "It answers /\u{2060}start@\u{2060}OtherBot.",
            ),
            (
                "It fetches https://example.com/a/b and a/b.",
                "It fetches https:/\u{2060}/\u{2060}example.com/\u{2060}a/\u{2060}b and \
                 a/\u{2060}b.",
            ),
            ("@@ //", "@\u{2060}@\u{2060} /\u{2060}/\u{2060}"),
        ] {
            let mut n = notice();
            n.cause = cause.into();
            let expected =
                GOLDEN_TEXT.replace("It is started with a flag it does not know.", joined);
            assert_eq!(telegram_of(&n), expected, "{cause}");
            // The same text once the joiners are taken out, and only Telegram's has them.
            assert_eq!(unjoin(&telegram_of(&n), &['@', '/']), text(&n));
            assert!(text(&n).contains(cause));
            // Slack's has them after '@' only.
            assert_eq!(
                slack_of(&n).contains(JOINER),
                cause.contains('@'),
                "{cause}"
            );
            assert!(!slack_of(&n).contains("/\u{2060}"), "{cause}");
            assert_eq!(json_body(&n, "0.2.0")["explanation"]["cause"], cause);
            check_bodies(&n, cause);
        }
        // Every field of the text, the names included (header 5, summary, cause, one step,
        // confidence and fingerprint), gets them.
        let n = filled("@x/", 1);
        assert_eq!(telegram_of(&n).matches("@\u{2060}x/\u{2060}").count(), 10);
        // A joiner already in the text is kept, and the one added follows it.
        let mut n = notice();
        n.cause = "@\u{2060}x".into();
        assert!(telegram_of(&n).contains("@\u{2060}\u{2060}x"));
        check_bodies(&n, "a joiner in the text");
    }

    #[test]
    fn slack_gets_a_word_joiner_after_every_at_sign() {
        // Slack turned a plain @here and @channel into mentions in a real post on 2026-10-30,
        // although the body says "parse": "none".
        let mut n = notice();
        n.summary = "@here @channel @everyone <!here> and https://example.com/a".into();
        let slack = slack_of(&n);
        assert!(
            slack.contains(
                "@\u{2060}here @\u{2060}channel @\u{2060}everyone &lt;!here&gt; and https://example.com/a"
            ),
            "{slack}"
        );
        assert!(!slack.contains("@here") && !slack.contains("@channel"));
        check_bodies(&n, "slack mentions");
    }

    #[test]
    fn a_text_full_of_at_signs_stays_within_4000_units_with_its_joiners() {
        // Over the cap: Telegram's text is cut earlier than the shared text, so that each '@' and
        // its joiner fit.
        let mut n = notice();
        n.summary = "@".repeat(5000);
        let t = text(&n);
        assert_eq!(units(&t), MAX_TEXT_UNITS);
        let telegram = telegram_of(&n);
        assert!(units(&telegram) <= MAX_TEXT_UNITS, "{}", units(&telegram));
        assert!(
            units(&telegram) >= MAX_TEXT_UNITS - 1,
            "cut too early: {}",
            units(&telegram)
        );
        let header = "kausa: crash loop (D001) in shop, Deployment web\n";
        let ats = (MAX_TEXT_UNITS - header.len()) / 2;
        assert_eq!(telegram, format!("{header}{}", "@\u{2060}".repeat(ats)));
        check_bodies(&n, "at signs over the cap");
        // Every field at the cap, in '@' and '/', and a cut that would fall between an '@' and
        // its joiner.
        for c in ["@", "/", "@/"] {
            let n = filled(&c.repeat(MAX_TEXT_UNITS), 5);
            check_bodies(&n, c);
        }
        let mut n = notice();
        n.summary = format!("a{}", "@".repeat(5000));
        check_bodies(&n, "an odd cut");
        // Right at the cap: a text of exactly 4000 units with its joiners is not cut.
        let mut n = notice();
        let room = MAX_TEXT_UNITS - units(&uncut(&n)) + n.summary.len();
        n.summary = "@".repeat(room / 2);
        assert_eq!(units(&uncut(&n)) + room / 2, MAX_TEXT_UNITS - room % 2);
        assert_eq!(unjoin(&telegram_of(&n), &['@', '/']), uncut(&n));
        n.summary.push('@');
        assert!(uncut(&n).starts_with(&unjoin(&telegram_of(&n), &['@', '/'])));
        assert_ne!(unjoin(&telegram_of(&n), &['@', '/']), uncut(&n));
    }

    #[test]
    fn chat_ids_are_integers_or_channel_usernames() {
        assert_eq!(
            parse_chat_id("-1001234567890"),
            Some(ChatId::Id(-1001234567890))
        );
        assert_eq!(parse_chat_id("12345"), Some(ChatId::Id(12345)));
        assert_eq!(parse_chat_id("-1"), Some(ChatId::Id(-1)));
        assert_eq!(
            parse_chat_id("9223372036854775807"),
            Some(ChatId::Id(i64::MAX))
        );
        assert_eq!(
            parse_chat_id("-9223372036854775808"),
            Some(ChatId::Id(i64::MIN))
        );
        for valid in [
            "@valid_name",
            "@abcde",
            "@A1234",
            "@kausa_alerts_2",
            "@abcdefghijklmnopqrstuvwxyz012345", // 32 characters after the '@'
        ] {
            let chat = parse_chat_id(valid).unwrap_or_else(|| panic!("{valid} is refused"));
            assert!(matches!(chat, ChatId::Username(_)), "{valid}");
            assert_eq!(telegram_body(&notice(), &chat)["chat_id"], valid);
        }
        for invalid in [
            "",
            "0",
            "-0",
            "+1",
            "--1",
            "1.5",
            "1e3",
            "0x10",
            " 1",
            "1 ",
            "1_000",
            "9223372036854775808",
            "-9223372036854775809",
            "99999999999999999999",
            "@",
            "@ab",
            "@abcd",
            "@1abcd",
            "@_abcd",
            "@abc-de",
            "@abc.de",
            "@abcdefghijklmnopqrstuvwxyz0123456", // 33 characters after the '@'
            "valid_name",
            "@valid_name ",
            "@vàlid_name",
            "-@valid_name",
        ] {
            assert_eq!(parse_chat_id(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn nothing_panics_on_odd_input() {
        let long = "\u{1F600}a\u{301}".repeat(10_000);
        for text in [
            "",
            "\u{0}",
            "\u{FEFF}",
            "\u{301}",
            "a\u{301}",
            "\u{1F600}",
            "\r\n",
            "\u{10FFFF}",
            "&&&<<<>>>",
            "&amp;",
            "@/@/",
            "\u{2060}",
            "/\u{2060}@",
            long.as_str(),
        ] {
            for steps in [0, 1, 5, 50] {
                let n = filled(text, steps);
                check_bodies(&n, &format!("{text:?} with {steps} steps"));
                let _ = telegram_body(&n, &ChatId::Username("@valid_name".into()));
                let _ = parse_chat_id(text);
            }
        }
    }
}
