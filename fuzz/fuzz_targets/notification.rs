//! The bodies of a notification (src/notification.rs) on any texts a tenant could shape through
//! the model: rendering them must never panic, the plain text stays within 4,000 UTF-16 units, the
//! Slack text within 20,000 with no `<` or `>` left and a word joiner after every `@`, Telegram's
//! text within 4,000 with one after every `@` and `/`, and the `json` body parses back
//! (docs/adr/0015-notifications.md). A `detected` message carries no text of a model, and the
//! `capped` one holds no `@`, `/`, `&`, `<` or `>` (docs/adr/0027-fact-statement-for-the-detections.md).
//! Chat ids are parsed from any text too.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/notification.rs"]
mod notification;

use libfuzzer_sys::fuzz_target;
use notification::{ChatId, Facts, JOINER, MAX_SLACK_UNITS, MAX_TEXT_UNITS, Message, Notice};
use serde_json::Value;

/// The fuzzer's input: the rule and the object, the other texts of the notice, the steps, how
/// many times the summary is repeated (so that the text can pass the cap within libFuzzer's input
/// size), a chat id's text, whether the message is `detected`, its facts, and a cap.
type Input<'a> = (
    (&'a str, &'a str, &'a str, &'a str, &'a str, &'a str),
    (&'a str, &'a str, &'a str, &'a str, &'a str),
    Vec<&'a str>,
    u8,
    &'a str,
    bool,
    (Option<&'a str>, Option<i32>, usize, bool, usize, Option<u64>),
    usize,
);

fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

fuzz_target!(|input: Input| {
    let (
        (detection_id, detection_name, api_version, kind, namespace, name),
        (fingerprint, first_seen, summary, cause, confidence),
        steps,
        repeat,
        chat,
        detected,
        (reason, exit_code, pods_seen, pods_seen_is_minimum, pods_now, revision),
        limit,
    ) = input;
    let message = if detected {
        Message::Detected
    } else {
        Message::Explained
    };
    let notice = Notice {
        message,
        detection_id: detection_id.to_owned(),
        detection_name: detection_name.to_owned(),
        api_version: api_version.to_owned(),
        kind: kind.to_owned(),
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        fingerprint: fingerprint.to_owned(),
        first_seen: first_seen.to_owned(),
        facts: Facts {
            reason: reason.map(str::to_owned),
            exit_code,
            pods_seen,
            pods_seen_is_minimum,
            pods_now,
            revision,
        },
        summary: summary.repeat(usize::from(repeat) + 1),
        cause: cause.to_owned(),
        steps: steps.iter().map(|s| (*s).to_owned()).collect(),
        confidence: confidence.to_owned(),
    };

    let text = notification::text(&notice);
    assert!(units(&text) <= MAX_TEXT_UNITS, "the text passes the cap");

    let slack = notification::slack_body(&notice);
    let slack = slack["text"].as_str().expect("the Slack text is a string");
    assert!(
        units(slack) <= MAX_SLACK_UNITS,
        "the Slack text passes the cap"
    );
    assert!(
        !slack.contains('<') && !slack.contains('>'),
        "an unescaped angle bracket"
    );
    let unescaped = slack
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    // Without the joiner after each '@', a prefix of the shared text.
    let mut plain = String::with_capacity(unescaped.len());
    let mut chars = unescaped.chars();
    while let Some(c) = chars.next() {
        plain.push(c);
        if c == '@' {
            assert_eq!(
                chars.next(),
                Some(JOINER),
                "an '@' without a joiner in Slack's text"
            );
        }
    }
    assert!(
        text.starts_with(&plain),
        "the Slack text is not the escaped shared text"
    );

    let json = notification::json_body(&notice, "0.2.0");
    let parsed: Value = serde_json::from_str(&json.to_string()).expect("the json body parses");
    assert_eq!(parsed, json, "the json body changes when parsed back");
    assert_eq!(json["type"], message.name());
    if detected {
        assert!(json.get("explanation").is_none(), "a detected body with an explanation");
        // The texts of the model are left out: the same text whatever they hold.
        let blank = Notice {
            summary: String::new(),
            cause: String::new(),
            steps: Vec::new(),
            confidence: String::new(),
            ..notice.clone()
        };
        assert_eq!(text, notification::text(&blank), "a model's text in a detected message");
    }

    let capped = notification::capped_text(limit);
    assert!(units(&capped) <= MAX_TEXT_UNITS, "the capped text passes the cap");
    assert!(
        !capped.contains(['@', '/', '&', '<', '>']),
        "the capped text holds a character the formats change"
    );
    let capped_json = notification::capped_json_body(limit, "0.2.0");
    assert_eq!(capped_json["type"], "capped");
    assert_eq!(notification::capped_slack_body(limit)["text"], capped.as_str());

    let parsed = notification::parse_chat_id(chat);
    match &parsed {
        Some(ChatId::Id(id)) => {
            assert_ne!(*id, 0, "a chat id of zero");
            assert_eq!(chat.parse::<i64>(), Ok(*id), "an odd integer");
            assert!(!chat.starts_with('+'), "a leading '+'");
        }
        Some(ChatId::Username(username)) => {
            assert_eq!(username, chat);
            assert!((6..=33).contains(&username.len()), "a username's length");
        }
        None => {}
    }
    let chat = parsed.unwrap_or(ChatId::Id(-1));
    assert_eq!(
        notification::capped_telegram_body(limit, &chat)["text"],
        capped.as_str()
    );
    let telegram = notification::telegram_body(&notice, &chat);
    let joined = telegram["text"]
        .as_str()
        .expect("the Telegram text is a string");
    assert!(
        units(joined) <= MAX_TEXT_UNITS,
        "Telegram's text passes the cap"
    );
    // Without the joiner after each '@' and '/', a prefix of the shared text.
    let mut plain = String::with_capacity(joined.len());
    let mut chars = joined.chars();
    while let Some(c) = chars.next() {
        plain.push(c);
        if c == '@' || c == '/' {
            assert_eq!(chars.next(), Some(JOINER), "an '@' or '/' without a joiner");
        }
    }
    assert!(
        text.starts_with(&plain),
        "Telegram's text is not the shared text"
    );
    assert!(telegram.get("parse_mode").is_none(), "a parse_mode");
});
