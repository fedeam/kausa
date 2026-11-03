//! Why a call to the model endpoint failed (src/model/pure.rs), on any status, body and
//! `Retry-After` an endpoint could send: classifying an error, reading a chat completion and
//! reading the delay must never panic. A delay is always within 30 to 600 seconds and comes only
//! from a `429` or a `503` with 1 to 10 ASCII digits; every cause has the class its name has in
//! the causes table; a body changes the cause only on a `4xx`, and makes a key error only on a
//! `400` whose error is a pair of `KEY_ERRORS`; and a `2xx` is a completion, a
//! `not_an_answer` or a `no_content`, nothing else (docs/adr/0020-check-the-endpoint.md).
#![no_main]

// `unfulfilled_lint_expectations`: the module expects some items to be dead in kausa until the
// code that reads them lands, and this target reads them.
#[allow(dead_code, unfulfilled_lint_expectations)]
#[path = "../../src/model/pure.rs"]
mod pure;

use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use pure::{Cause, Class, Completion};

/// The fuzzer's input: a status, a body, and the bytes of a `Retry-After` header if any.
type Input<'a> = (u16, &'a [u8], Option<&'a [u8]>);

/// The class of each cause by name, as ADR 0020's table gives it.
fn class_of(name: &str) -> Option<Class> {
    Some(match name {
        "connection" | "timeout" | "rate_limited" | "server_error" => Class::Transient,
        "redirect" | "not_an_answer" | "quota" | "key" | "not_found" => Class::Configuration,
        "no_content" | "too_large" | "rejected" => Class::Rejected,
        _ => return None,
    })
}

/// Every cause has a known name, the class that name has, a class name, and a hint either way.
fn check_cause(cause: Cause) {
    let name = cause.as_str();
    assert_eq!(
        class_of(name),
        Some(cause.class()),
        "a cause with the wrong class"
    );
    assert!(
        Cause::ALL.iter().any(|known| known.as_str() == name),
        "a cause outside Cause::ALL"
    );
    assert!(["transient", "configuration", "rejected"].contains(&cause.class().as_str()));
    assert!(!cause.hint(true).is_empty() && !cause.hint(false).is_empty());
}

/// The cause of a non-`2xx` answer: fixed by the status, except that a body can choose between
/// two causes on a `429` and on a `4xx` without a cause of its own, and three on a `400`.
fn check_classify(status: u16, body: &[u8]) {
    let cause = pure::classify(status, body);
    check_cause(cause);
    assert_eq!(
        cause,
        pure::classify(status, body),
        "classify is not stable"
    );
    let expected: &[Cause] = match status {
        200..=299 => &[Cause::NotAnAnswer],
        408 => &[Cause::Timeout],
        300..=399 => &[Cause::Redirect],
        429 => &[Cause::Quota, Cause::RateLimited],
        402 => &[Cause::Quota],
        401 | 403 => &[Cause::Key],
        404 | 405 | 410 => &[Cause::NotFound],
        413 => &[Cause::TooLarge],
        400 => &[Cause::Key, Cause::TooLarge, Cause::Rejected],
        400..=499 => &[Cause::TooLarge, Cause::Rejected],
        _ => &[Cause::ServerError],
    };
    assert!(
        expected.contains(&cause),
        "a cause the status does not allow"
    );
    if !(400..=499).contains(&status) || matches!(status, 401..=405 | 408 | 410 | 413) {
        assert_eq!(
            cause,
            pure::classify(status, b""),
            "a body changed a cause fixed by the status"
        );
    }
}

/// A `400` is a key error only when its `error.status` and `error.message`, read the way the code
/// reads them, are one of the pairs, byte for byte: never from a quote of the request.
fn check_key_error(body: &[u8]) {
    let pair = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .map(|value| match value {
            serde_json::Value::Array(items) => items.into_iter().next().unwrap_or_default(),
            other => other,
        })
        .map(|first| {
            let field = |key: &str| first["error"][key].as_str().map(str::to_owned);
            (field("status"), field("message"))
        });
    let listed = pair.is_some_and(|(status, message)| {
        pure::KEY_ERRORS
            .iter()
            .any(|&(s, m)| status.as_deref() == Some(s) && message.as_deref() == Some(m))
    });
    assert_eq!(
        pure::classify(400, body) == Cause::Key,
        listed,
        "a key error that is not a listed pair, or a listed pair that is not a key error"
    );
}

/// A `2xx` within the cap: a completion only from JSON, else `not_an_answer` or `no_content`.
fn check_completion(body: &[u8]) {
    match pure::completion(body) {
        Ok(Completion { .. }) => assert!(
            serde_json::from_slice::<serde_json::Value>(body).is_ok(),
            "a completion from a body that is not JSON"
        ),
        Err(cause @ (Cause::NotAnAnswer | Cause::NoContent(_))) => check_cause(cause),
        Err(_) => panic!("a completion error that is neither not_an_answer nor no_content"),
    }
}

/// A delay only on a `429` or a `503`, only from 1 to 10 ASCII digits, and always within the cap.
fn check_retry_after(status: u16, header: Option<&[u8]>) {
    let delay = pure::retry_after(status, header);
    let digits = header.filter(|h| (1..=10).contains(&h.len()) && h.iter().all(u8::is_ascii_digit));
    assert_eq!(
        delay.is_some(),
        matches!(status, 429 | 503) && digits.is_some(),
        "a delay read where none is due, or none where one is"
    );
    if let Some(delay) = delay {
        // The literal bounds of ADR 0020, not the constants, so that a changed constant fails too.
        assert!(
            (Duration::from_secs(30)..=Duration::from_secs(600)).contains(&delay),
            "a delay outside 30 to 600 seconds"
        );
    }
}

fuzz_target!(|input: Input| {
    let (status, body, header) = input;

    // The fuzzed status, and the statuses whose cause the body can change, so that every body is
    // read as an error body whatever the status.
    for status in [status, 400, 422, 429] {
        check_classify(status, body);
    }
    check_key_error(body);
    check_completion(body);
    for status in [status, 429, 503] {
        check_retry_after(status, header);
    }
});
