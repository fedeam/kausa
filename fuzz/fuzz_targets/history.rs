//! The explanation history (src/history/pure.rs) on any stored object a tenant could write:
//! reading it back, merging a new explanation into it and planning the sweep must never panic.
//! Every text kausa writes is one `prompt::clean` leaves as it is, so its own entries survive the
//! next read; no time it writes is later than now; and `prompt::clean` is a fixed point on every
//! text it is given (docs/adr/0014-explanation-history.md).
#![no_main]

#[allow(dead_code)]
#[path = "../../src/prompt.rs"]
mod prompt;

#[allow(dead_code)]
#[path = "../../src/history/pure.rs"]
mod history;

use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};

use prompt::{Explanation, MAX_CAUSE, MAX_STEP, MAX_STEPS, MAX_SUMMARY};

const FINGERPRINT: &str = "0123456789abcdef";
const CONFIDENCES: [&str; 4] = ["low", "medium", "high", "unknown"];

/// The fuzzer's input: a stored value (JSON if it parses), the texts of a new explanation, a
/// confidence pick, a cap for `clean`, the digits of four times, and a stored count.
type Input<'a> = (
    &'a [u8],
    (&'a str, &'a str, Vec<&'a str>, &'a str),
    u8,
    u8,
    [u8; 56],
    u64,
);

/// A time in kausa's one format from 14 bytes, each read as a digit. Not always a calendar date,
/// which the pure part never needs: it checks the format and compares as strings.
fn time(bytes: &[u8]) -> String {
    let d: Vec<char> = bytes.iter().map(|b| char::from(b'0' + b % 10)).collect();
    let part = |range: std::ops::Range<usize>| d[range].iter().collect::<String>();
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        part(0..4),
        part(4..6),
        part(6..8),
        part(8..10),
        part(10..12),
        part(12..14)
    )
}

/// What kausa builds must hold: a `status` with an `occurrences` of at least 1 and `lastTime` now;
/// at most five entries, each with texts `clean` leaves as they are, a known confidence, a count of
/// at least 1, and times not later than now and (its `lastTime`) not before the cutoff. Read back
/// as a stored object, every entry and the `occurrences` survive.
fn check(status: &Value, now: &str, cutoff: &str) {
    let occurrences = status["occurrences"]
        .as_u64()
        .expect("an integer occurrences");
    assert!((1..=u64::from(u32::MAX)).contains(&occurrences));
    assert_eq!(status["lastTime"].as_str(), Some(now));
    let entries = status["entries"].as_array().expect("a list of entries");
    assert!(entries.len() <= history::MAX_ENTRIES);
    let clean = |text: &Value, cap: usize| {
        let text = text.as_str().expect("a text");
        assert_eq!(
            prompt::clean(text, cap),
            text,
            "a written text changes under clean"
        );
    };
    for entry in entries {
        clean(&entry["summary"], MAX_SUMMARY);
        clean(&entry["cause"], MAX_CAUSE);
        let steps = entry["steps"].as_array().expect("a list of steps");
        assert!(steps.len() <= MAX_STEPS);
        for step in steps {
            clean(step, MAX_STEP);
        }
        clean(&entry["model"], usize::MAX);
        clean(&entry["promptVersion"], usize::MAX);
        let confidence = entry["confidence"].as_str().expect("a confidence");
        assert!(CONFIDENCES[..3].contains(&confidence));
        assert!(entry["count"].as_u64().is_some_and(|n| n >= 1));
        let first = entry["firstTime"].as_str().expect("a firstTime");
        let last = entry["lastTime"].as_str().expect("a lastTime");
        assert!(first <= now, "a firstTime later than now");
        assert!(last <= now, "a lastTime later than now");
        assert!(last >= cutoff, "a lastTime before the cutoff");
    }
    let object = json!({
        "metadata": {
            "name": history::name(FINGERPRINT),
            "labels": {history::LABEL: history::MANAGER},
            "resourceVersion": "1",
        },
        "status": status,
    });
    let stored = history::parse_stored(&object);
    assert_eq!(
        stored.entries.len(),
        entries.len(),
        "a written entry is dropped on reading"
    );
    assert_eq!(stored.occurrences.map(u64::from), Some(occurrences));
}

fuzz_target!(|input: Input| {
    let (raw, (summary, cause, steps, model), pick, cap, digits, count) = input;
    let mut times: Vec<String> = digits.chunks(14).map(time).collect();
    let (now, cutoff) = if times[0] >= times[1] {
        (times[0].clone(), times[1].clone())
    } else {
        (times[1].clone(), times[0].clone())
    };
    let (stored_first, stored_last) = (times.remove(2), times.remove(2));

    // `clean` is a fixed point on every text, at the caps kausa uses and at a short one, which
    // cuts through markers.
    let lossy = String::from_utf8_lossy(raw);
    let texts = [summary, cause, model, lossy.as_ref()];
    for text in texts.iter().copied().chain(steps.iter().copied()) {
        for max in [MAX_SUMMARY, MAX_CAUSE, MAX_STEP, usize::from(cap)] {
            let once = prompt::clean(text, max);
            assert_eq!(prompt::clean(&once, max), once, "clean is not idempotent");
        }
    }

    let explanation = Explanation {
        summary: summary.to_owned(),
        cause: cause.to_owned(),
        steps: steps.iter().map(|s| (*s).to_owned()).collect(),
        confidence: CONFIDENCES[usize::from(pick % 3)].to_owned(),
    };
    let spec = history::spec_of("D001", "apps/v1", "Deployment", "crasher");

    let body = history::create_body(FINGERPRINT, &spec, &explanation, model, &now);
    check(&body["status"], &now, &cutoff);

    // A stored entry made of the same texts, in the one time format: it is kept only if `clean`
    // leaves them as they are, its confidence is known, its count is in range and its times are
    // not later than now.
    let seeded = json!({
        "summary": summary,
        "cause": cause,
        "steps": steps,
        "confidence": CONFIDENCES[usize::from(pick / 3 % 4)],
        "model": model,
        "promptVersion": prompt::PROMPT_VERSION,
        "firstTime": stored_first,
        "lastTime": stored_last,
        "count": count,
    });
    let value =
        serde_json::from_slice::<Value>(raw).unwrap_or_else(|_| Value::from(lossy.as_ref()));
    let shaped = |status: Value| {
        json!({
            "metadata": {
                "name": history::name(FINGERPRINT),
                "namespace": "e2e-shop",
                "labels": {history::LABEL: history::MANAGER},
                "resourceVersion": "7",
                "creationTimestamp": stored_first,
            },
            "spec": {"rule": "D001", "target": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "crasher"}},
            "status": status,
        })
    };
    let objects = [
        value.clone(),
        json!({"status": value}),
        json!({"status": {"entries": value}}),
        shaped(json!({"occurrences": count, "lastTime": stored_last, "entries": [seeded, value]})),
        shaped(json!({"occurrences": value, "lastTime": value, "entries": [value, seeded]})),
    ];

    for object in &objects {
        let stored = history::parse_stored(object);
        let _ = history::check_identity(&stored, FINGERPRINT, &spec);
        let status = history::merge(&stored, &explanation, model, &now, &cutoff);
        let patch = history::patch_body("7", &status);
        check(&patch["status"], &now, &cutoff);
    }

    let mut plan = history::SweepPlan::new(&now, &cutoff);
    match &value {
        Value::Array(items) => {
            let _ = plan.page(items);
        }
        _ => {
            let _ = plan.page(&objects);
        }
    }
    let _ = plan.page(&objects);
    assert!(plan.held() <= history::MAX_OBJECTS);
});
