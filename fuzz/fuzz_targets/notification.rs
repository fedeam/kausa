//! The bodies of a notification (src/notification.rs) on any texts a tenant could shape through
//! the model: rendering them must never panic, the plain text stays within 4,000 UTF-16 units, the
//! Slack text within 20,000 with no `<` or `>` left, Telegram's text within 4,000 with a word
//! joiner after every `@` and `/`, and the `json` body parses back
//! (docs/adr/0015-notifications.md). Chat ids are parsed from any text too.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/notification.rs"]
mod notification;

use libfuzzer_sys::fuzz_target;
use notification::{ChatId, JOINER, MAX_SLACK_UNITS, MAX_TEXT_UNITS, Notice};
use serde_json::Value;

/// The fuzzer's input: the rule and the object, the other texts of the notice, the steps, how
/// many times the summary is repeated (so that the text can pass the cap within libFuzzer's input
/// size), and a chat id's text.
type Input<'a> = (
    (&'a str, &'a str, &'a str, &'a str, &'a str, &'a str),
    (&'a str, &'a str, &'a str, &'a str, &'a str),
    Vec<&'a str>,
    u8,
    &'a str,
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
    ) = input;
    let notice = Notice {
        detection_id: detection_id.to_owned(),
        detection_name: detection_name.to_owned(),
        api_version: api_version.to_owned(),
        kind: kind.to_owned(),
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        fingerprint: fingerprint.to_owned(),
        first_seen: first_seen.to_owned(),
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
    assert_eq!(unescaped, text, "the Slack text is not the escaped text");

    let json = notification::json_body(&notice, "0.2.0");
    let parsed: Value = serde_json::from_str(&json.to_string()).expect("the json body parses");
    assert_eq!(parsed, json, "the json body changes when parsed back");

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
