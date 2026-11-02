//! Why a call to the model endpoint failed, from what the endpoint answered
//! (docs/adr/0020-check-the-endpoint.md): the cause and its class from the status and two fixed
//! keys of the error body, the content of a chat completion, and a clamped `Retry-After`.
//!
//! Pure: no I/O, no clock and no `crate::` import, so that the fuzz target `endpoint_error`
//! includes this file by path; it depends only on `std` and `serde_json`, and never panics.

use std::time::Duration;

use serde_json::Value;

/// Why a call failed, as kausa names it in its log, the check's line and its tests. Each cause is a
/// constant kausa defines: nothing read from the endpoint's answer but the cause comes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// A connect, DNS or TLS failure: the endpoint never received the request.
    Connection,
    /// No answer within the timeout, or `408`.
    Timeout,
    /// Any `3xx`: kausa follows no redirect.
    Redirect,
    /// A `2xx` that is not JSON, has no `choices` array, or is over the cap.
    NotAnAnswer,
    /// A `2xx` chat completion whose first message has no string `content`.
    NoContent(Missing),
    /// `429` without a quota code.
    RateLimited,
    /// `402`, or `429` with a code in [`QUOTA_CODES`].
    Quota,
    /// `401` or `403`.
    Key,
    /// `404`, `405` or `410`.
    NotFound,
    /// `413`, or another `4xx` with a code in [`TOO_LARGE_CODES`].
    TooLarge,
    /// Any other `4xx`.
    Rejected,
    /// A `5xx`, any status outside `200`-`499`, or an answer cut short.
    ServerError,
}

/// Why a chat completion has no content, read only from fixed keys: never the model's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// `choices[0].message.refusal` is present and not null.
    Refusal,
    /// `choices[0].finish_reason` is `content_filter`.
    ContentFilter,
    /// `choices[0].finish_reason` is `length`: the output tokens ran out first.
    Length,
    /// Anything else.
    Other,
}

/// What a cause means for the breaker and for the incident whose call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Worth one more try, and counted towards opening the breaker.
    Transient,
    /// The endpoint, the key, the model or the account is wrong: never retried, opens the breaker.
    Configuration,
    /// The endpoint refused this request, which a tenant shapes: never retried, moves no breaker.
    Rejected,
}

/// The codes of a `429` that mean the account has no quota or credit left (seen on 2026-10-30).
pub const QUOTA_CODES: &[&str] = &["insufficient_quota"];

/// The codes of a `4xx` that mean the request is too large for the model (a hypothesis, harmless:
/// both `too_large` and `rejected` are neutral for the breaker).
pub const TOO_LARGE_CODES: &[&str] = &["context_length_exceeded"];

/// The shortest delay a `Retry-After` gives.
pub const RETRY_AFTER_MIN: Duration = Duration::from_secs(30);

/// The longest delay a `Retry-After` gives.
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(600);

/// The longest `error.code`, `error.type` or `finish_reason` that is compared, in bytes.
pub const MAX_CODE_BYTES: usize = 64;

/// The most digits of a `Retry-After` in seconds.
const MAX_RETRY_AFTER_DIGITS: usize = 10;

/// The hint of the endpoint check when the model answered, but its answer failed validation.
pub const INVALID_ANSWER_HINT: &str = "the model answered, but not with kausa's JSON: try another \
     model, or model.jsonSchema=false (--no-json-schema) if the server ignores response_format";

impl Cause {
    /// Every cause once, in the order of the causes table of the command-line reference, with
    /// `NoContent(Missing::Other)` for `no_content`.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read only by the tests and the fuzz target")
    )]
    pub const ALL: [Cause; 12] = [
        Cause::Connection,
        Cause::Timeout,
        Cause::Redirect,
        Cause::NotAnAnswer,
        Cause::NoContent(Missing::Other),
        Cause::RateLimited,
        Cause::Quota,
        Cause::Key,
        Cause::NotFound,
        Cause::TooLarge,
        Cause::Rejected,
        Cause::ServerError,
    ];

    pub fn class(self) -> Class {
        match self {
            Cause::Connection | Cause::Timeout | Cause::RateLimited | Cause::ServerError => {
                Class::Transient
            }
            Cause::Redirect | Cause::NotAnAnswer | Cause::Quota | Cause::Key | Cause::NotFound => {
                Class::Configuration
            }
            Cause::NoContent(_) | Cause::TooLarge | Cause::Rejected => Class::Rejected,
        }
    }

    /// The name in logs, the check's line and the docs.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Connection => "connection",
            Cause::Timeout => "timeout",
            Cause::Redirect => "redirect",
            Cause::NotAnAnswer => "not_an_answer",
            Cause::NoContent(_) => "no_content",
            Cause::RateLimited => "rate_limited",
            Cause::Quota => "quota",
            Cause::Key => "key",
            Cause::NotFound => "not_found",
            Cause::TooLarge => "too_large",
            Cause::Rejected => "rejected",
            Cause::ServerError => "server_error",
        }
    }

    /// What to check, naming the flag and the chart value; `json_schema` says whether the request
    /// asked for structured output. The `800` of `no_content` is `prompt::MAX_TOKENS` (a test in
    /// src/model.rs ties them).
    pub fn hint(self, json_schema: bool) -> &'static str {
        match self {
            Cause::Connection => {
                "cannot connect: check model.endpoint (--endpoint), DNS, the certificate and \
                 networkPolicy.endpoint"
            }
            Cause::Timeout => {
                "no answer within the timeout: the model may be overloaded or slow on this \
                 hardware; raise model.timeoutSeconds (--model-timeout) or try later"
            }
            Cause::Redirect => {
                "the endpoint redirects and kausa follows no redirect: put the final URL in \
                 model.endpoint (--endpoint)"
            }
            Cause::NotAnAnswer => {
                "the URL does not serve the OpenAI Chat Completions API: put the base URL (often \
                 ending in /v1) in model.endpoint (--endpoint); kausa appends /chat/completions"
            }
            Cause::NoContent(_) => {
                "the model answered without text: it refused, a filter removed it, or it spent \
                 kausa's 800 output tokens before answering (reason says which); with the \
                 check's synthetic request, try another model, or one that does not reason at \
                 length"
            }
            Cause::RateLimited => {
                "the provider limits the rate of this key: kausa waits and tries again"
            }
            Cause::Quota => {
                "the provider's account has no quota or credit left: add credit, or use another \
                 key (model.apiKey.existingSecret, --api-key-file)"
            }
            Cause::Key => {
                "the endpoint refused the API key: check the Secret in \
                 model.apiKey.existingSecret (--api-key-file) and the key's permissions for this \
                 model"
            }
            Cause::NotFound => {
                "the model or the URL is wrong, or the model is no longer offered: check \
                 model.name (--model) and that model.endpoint is the base URL"
            }
            Cause::TooLarge => {
                "the model refuses a request of this size: lower budget.maxInputTokens \
                 (--max-input-tokens) or use a model with a larger context"
            }
            Cause::Rejected if json_schema => {
                "the endpoint refused the request: if it does not support structured output, \
                 set model.jsonSchema=false (--no-json-schema); see detail"
            }
            Cause::Rejected => "the endpoint refused the request: see detail",
            Cause::ServerError => {
                "the provider is failing: kausa holds calls and tries again later"
            }
        }
    }
}

impl Class {
    /// The name in logs and the check's line.
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Transient => "transient",
            Class::Configuration => "configuration",
            Class::Rejected => "rejected",
        }
    }
}

/// The cause of a non-`2xx` answer, from its status and, on a `429` or another `4xx`, the
/// `error.code` and `error.type` of its body (of the first element when the body is an array).
/// A `2xx` is not an error: it gives `NotAnAnswer`, and [`completion`] reads it.
pub fn classify(status: u16, body: &[u8]) -> Cause {
    match status {
        200..=299 => Cause::NotAnAnswer,
        408 => Cause::Timeout,
        300..=399 => Cause::Redirect,
        429 if has_code(body, QUOTA_CODES) => Cause::Quota,
        429 => Cause::RateLimited,
        402 => Cause::Quota,
        401 | 403 => Cause::Key,
        404 | 405 | 410 => Cause::NotFound,
        413 => Cause::TooLarge,
        400..=499 if has_code(body, TOO_LARGE_CODES) => Cause::TooLarge,
        400..=499 => Cause::Rejected,
        _ => Cause::ServerError,
    }
}

/// Whether `error.code` or `error.type` of the body is one of `codes`, byte for byte. Anything
/// that is not JSON, nests deeper than serde_json reads, or is not a short string is no code.
fn has_code(body: &[u8], codes: &[&str]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let first = match &value {
        Value::Array(items) => match items.first() {
            Some(item) => item,
            None => return false,
        },
        other => other,
    };
    ["code", "type"]
        .iter()
        .any(|key| short_str(&first["error"][*key]).is_some_and(|code| codes.contains(&code)))
}

/// A string of at most `MAX_CODE_BYTES` bytes.
fn short_str(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| s.len() <= MAX_CODE_BYTES)
}

/// What a chat completion carries that kausa reads: the text of the first choice and the token
/// counts the endpoint reported.
#[derive(Debug, PartialEq, Eq)]
pub struct Completion {
    pub content: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

/// A `2xx` answer within the cap: the completion, `NotAnAnswer` when it is not JSON or has no
/// `choices` array, or `NoContent` when the first choice has no string `message.content`. Only the
/// kind of missing content is read, from fixed keys; never the refusal's text.
pub fn completion(body: &[u8]) -> Result<Completion, Cause> {
    let value: Value = serde_json::from_slice(body).map_err(|_| Cause::NotAnAnswer)?;
    if !value["choices"].is_array() {
        return Err(Cause::NotAnAnswer);
    }
    let choice = &value["choices"][0];
    if let Some(content) = choice["message"]["content"].as_str() {
        return Ok(Completion {
            content: content.to_owned(),
            prompt_tokens: value["usage"]["prompt_tokens"].as_u64(),
            completion_tokens: value["usage"]["completion_tokens"].as_u64(),
        });
    }
    let kind = if !choice["message"]["refusal"].is_null() {
        Missing::Refusal
    } else {
        match short_str(&choice["finish_reason"]) {
            Some("content_filter") => Missing::ContentFilter,
            Some("length") => Missing::Length,
            _ => Missing::Other,
        }
    };
    Err(Cause::NoContent(kind))
}

/// The delay a `Retry-After` header asks for, on a `429` or a `503` only: delta-seconds of 1 to 10
/// ASCII digits, clamped to `RETRY_AFTER_MIN`..=`RETRY_AFTER_MAX`. Anything else (a date, a sign,
/// white space) is ignored.
pub fn retry_after(status: u16, header: Option<&[u8]>) -> Option<Duration> {
    if status != 429 && status != 503 {
        return None;
    }
    let digits = header?;
    if digits.is_empty()
        || digits.len() > MAX_RETRY_AFTER_DIGITS
        || !digits.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let seconds = digits
        .iter()
        .fold(0u64, |n, d| n * 10 + u64::from(d - b'0'));
    Some(Duration::from_secs(seconds).clamp(RETRY_AFTER_MIN, RETRY_AFTER_MAX))
}

#[cfg(test)]
mod tests {
    // Acceptance tests for docs/adr/0020-check-the-endpoint.md, written before the code. They use
    // this API, which does not exist yet:
    //
    // - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Cause { Connection, Timeout,
    //   Redirect, NotAnAnswer, NoContent(Missing), RateLimited, Quota, Key, NotFound, TooLarge,
    //   Rejected, ServerError }`, the same derives on `pub enum Missing { Refusal,
    //   ContentFilter, Length, Other }` and `pub enum Class { Transient, Configuration, Rejected
    //   }`;
    // - `Cause::ALL: [Cause; 12]` in that order (`NoContent(Missing::Other)` for `no_content`),
    //   `Cause::class(self) -> Class`, `Cause::as_str(self) -> &'static str`,
    //   `Cause::hint(self, json_schema: bool) -> &'static str`, `Class::as_str(self) -> &'static
    //   str` and `pub const INVALID_ANSWER_HINT: &str`;
    // - `pub fn classify(status: u16, body: &[u8]) -> Cause` (a non-2xx answer; a 2xx gives
    //   `NotAnAnswer`), `pub fn completion(body: &[u8]) -> Result<Completion, Cause>` with
    //   `pub struct Completion { pub content: String, pub prompt_tokens: Option<u64>, pub
    //   completion_tokens: Option<u64> }`, and `pub fn retry_after(status: u16, header:
    //   Option<&[u8]>) -> Option<Duration>`;
    // - `QUOTA_CODES`, `TOO_LARGE_CODES`, `RETRY_AFTER_MIN`, `RETRY_AFTER_MAX` and
    //   `MAX_CODE_BYTES`.
    //
    // Bodies are invented; none was captured from a provider but the shape of the quota answer
    // (journal 2026-10-30).

    use super::*;

    use std::time::Duration;

    /// An OpenAI-shaped error body with `value` under `field` of `error`.
    fn error_body(field: &str, value: &str) -> String {
        serde_json::json!({"error": {"message": "an invented message", field: value}}).to_string()
    }

    fn classified(status: u16, body: &str) -> Cause {
        classify(status, body.as_bytes())
    }

    #[test]
    fn the_status_alone_gives_the_cause_of_an_empty_body() {
        let cases: &[(u16, Cause)] = &[
            (408, Cause::Timeout),
            (300, Cause::Redirect),
            (301, Cause::Redirect),
            (302, Cause::Redirect),
            (307, Cause::Redirect),
            (308, Cause::Redirect),
            (399, Cause::Redirect),
            (429, Cause::RateLimited),
            (402, Cause::Quota),
            (401, Cause::Key),
            (403, Cause::Key),
            (404, Cause::NotFound),
            (405, Cause::NotFound),
            (410, Cause::NotFound),
            (413, Cause::TooLarge),
            (400, Cause::Rejected),
            (409, Cause::Rejected),
            (415, Cause::Rejected),
            (418, Cause::Rejected),
            (422, Cause::Rejected),
            (451, Cause::Rejected),
            (499, Cause::Rejected),
            (500, Cause::ServerError),
            (502, Cause::ServerError),
            (503, Cause::ServerError),
            (504, Cause::ServerError),
            (599, Cause::ServerError),
            // Any status outside 200-499: 1xx and above 599.
            (100, Cause::ServerError),
            (101, Cause::ServerError),
            (600, Cause::ServerError),
            (999, Cause::ServerError),
            (0, Cause::ServerError),
            (u16::MAX, Cause::ServerError),
        ];
        for &(status, cause) in cases {
            assert_eq!(classify(status, b""), cause, "{status}");
        }
    }

    #[test]
    fn a_2xx_given_to_classify_is_not_an_answer() {
        for status in [200, 201, 204, 299] {
            assert_eq!(classify(status, b"{}"), Cause::NotAnAnswer, "{status}");
        }
    }

    #[test]
    fn a_quota_code_on_a_429_is_quota_wherever_the_provider_puts_it() {
        let under_code = error_body("code", "insufficient_quota");
        let under_type = error_body("type", "insufficient_quota");
        let both =
            r#"{"error":{"message":"m","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        let in_array = format!("[{under_code}]");
        let type_beside_a_null_code = r#"{"error":{"code":null,"type":"insufficient_quota"}}"#;
        for body in [
            under_code.as_str(),
            under_type.as_str(),
            both,
            in_array.as_str(),
            type_beside_a_null_code,
        ] {
            assert_eq!(classified(429, body), Cause::Quota, "{body}");
        }
        assert!(QUOTA_CODES.contains(&"insufficient_quota"));
    }

    #[test]
    fn only_the_fixed_keys_of_the_first_element_are_read() {
        let quota = error_body("code", "insufficient_quota");
        let other = error_body("code", "rate_limit_exceeded");
        for body in [
            // Only the first element of an array.
            format!("[{other},{quota}]"),
            // Not under `error`.
            r#"{"code":"insufficient_quota"}"#.to_owned(),
            r#"{"data":{"error":{"code":"insufficient_quota"}}}"#.to_owned(),
            // Another key of `error`.
            error_body("message", "insufficient_quota"),
            error_body("status", "insufficient_quota"),
            // `error` as a string.
            r#"{"error":"insufficient_quota"}"#.to_owned(),
            // Compared byte for byte.
            error_body("code", "Insufficient_Quota"),
            error_body("code", " insufficient_quota"),
            error_body("code", "insufficient_quota "),
            error_body("code", "insufficient_quota_x"),
            // Not JSON: trailing text.
            format!("{quota} and more"),
        ] {
            assert_eq!(classified(429, &body), Cause::RateLimited, "{body}");
        }
    }

    #[test]
    fn a_code_that_is_not_a_string_is_no_code() {
        for body in [
            r#"{"error":{"code":429}}"#,
            r#"{"error":{"code":["insufficient_quota"]}}"#,
            r#"{"error":{"code":{"value":"insufficient_quota"}}}"#,
            r#"{"error":{"code":true,"type":null}}"#,
            r#"{"error":{"type":["context_length_exceeded"]}}"#,
        ] {
            assert_eq!(classified(429, body), Cause::RateLimited, "{body}");
            assert_eq!(classified(400, body), Cause::Rejected, "{body}");
        }
    }

    #[test]
    fn codes_of_64_and_65_bytes_are_read_or_ignored_without_harm() {
        assert_eq!(MAX_CODE_BYTES, 64);
        let longest = "a".repeat(MAX_CODE_BYTES);
        let too_long = "a".repeat(MAX_CODE_BYTES + 1);
        // A listed code padded to 65 bytes, and multibyte text around the limit.
        let padded = format!("insufficient_quota{}", " ".repeat(65 - 18));
        let multibyte = "é".repeat(33);
        for code in [&longest, &too_long, &padded, &multibyte] {
            for field in ["code", "type"] {
                let body = error_body(field, code);
                assert_eq!(classified(429, &body), Cause::RateLimited, "{field}");
                assert_eq!(classified(400, &body), Cause::Rejected, "{field}");
            }
        }
    }

    #[test]
    fn a_quota_code_on_another_status_is_not_quota() {
        let quota = error_body("code", "insufficient_quota");
        assert_eq!(classified(400, &quota), Cause::Rejected);
        assert_eq!(classified(403, &quota), Cause::Key);
        assert_eq!(classified(503, &quota), Cause::ServerError);
    }

    #[test]
    fn a_too_large_code_on_a_4xx_is_too_large_but_not_on_a_429() {
        assert!(TOO_LARGE_CODES.contains(&"context_length_exceeded"));
        for field in ["code", "type"] {
            let body = error_body(field, "context_length_exceeded");
            assert_eq!(classified(400, &body), Cause::TooLarge, "{field}");
            assert_eq!(classified(422, &body), Cause::TooLarge, "{field}");
            assert_eq!(classified(429, &body), Cause::RateLimited, "{field}");
            // The status rows above the code win.
            assert_eq!(classified(401, &body), Cause::Key, "{field}");
            assert_eq!(classified(404, &body), Cause::NotFound, "{field}");
            assert_eq!(classified(500, &body), Cause::ServerError, "{field}");
        }
    }

    #[test]
    fn bodies_that_are_not_json_or_nest_deeply_give_the_status_alone() {
        let deep_array = format!("{}{}", "[".repeat(200), "]".repeat(200));
        let deep_object = format!("{}1{}", r#"{"error":"#.repeat(200), "}".repeat(200));
        let bodies: [&[u8]; 6] = [
            b"",
            b"\xff\xfe\x00not utf-8",
            b"<html><body>Too Many Requests</body></html>",
            deep_array.as_bytes(),
            deep_object.as_bytes(),
            b"[]",
        ];
        for body in bodies {
            assert_eq!(classify(429, body), Cause::RateLimited);
            assert_eq!(classify(400, body), Cause::Rejected);
            assert_eq!(classify(402, body), Cause::Quota);
            assert_eq!(classify(503, body), Cause::ServerError);
        }
    }

    // `completion`: a 2xx answer within the cap.

    fn completion_cause(body: &str) -> Option<Cause> {
        completion(body.as_bytes()).err()
    }

    #[test]
    fn a_2xx_that_is_not_a_chat_completion_is_not_an_answer() {
        for body in [
            "OK",
            "",
            "<!doctype html><html><body>Welcome</body></html>",
            "{}",
            r#"{"choices":null}"#,
            r#"{"choices":{}}"#,
            r#"{"choices":"none"}"#,
            r#"{"object":"list","data":[]}"#,
            // An array at the top level has no `choices` (the first-element rule is for errors).
            r#"[{"choices":[{"message":{"content":"x"}}]}]"#,
            "null",
        ] {
            assert_eq!(completion_cause(body), Some(Cause::NotAnAnswer), "{body}");
        }
        assert_eq!(
            completion(b"\xff\xfe").err(),
            Some(Cause::NotAnAnswer),
            "invalid UTF-8"
        );
    }

    #[test]
    fn a_chat_completion_without_string_content_is_no_content_of_its_kind() {
        let other = Some(Cause::NoContent(Missing::Other));
        for body in [
            r#"{"choices":[]}"#,
            r#"{"choices":[1]}"#,
            r#"{"choices":[{}]}"#,
            r#"{"choices":[{"message":{}}]}"#,
            r#"{"choices":[{"message":null}]}"#,
            r#"{"choices":[{"message":{"content":null}}]}"#,
            r#"{"choices":[{"message":{"content":["a","b"]}}]}"#,
            r#"{"choices":[{"message":{"content":42}}]}"#,
            r#"{"choices":[{"message":{"content":null},"finish_reason":"stop"}]}"#,
            r#"{"choices":[{"message":{"content":null,"refusal":null},"finish_reason":"STOP"}]}"#,
            r#"{"choices":[{"message":{"content":null},"finish_reason":42}]}"#,
            // Compared byte for byte.
            r#"{"choices":[{"message":{"content":null},"finish_reason":"LENGTH"}]}"#,
            r#"{"choices":[{"message":{"content":null},"finish_reason":"length "}]}"#,
        ] {
            assert_eq!(completion_cause(body), other, "{body}");
        }
        let refusal = r#"{"choices":[{"message":{"content":null,"refusal":"REFUSAL-CANARY"},"finish_reason":"stop"}]}"#;
        assert_eq!(
            completion_cause(refusal),
            Some(Cause::NoContent(Missing::Refusal))
        );
        // A refusal that is not a string still counts: present and not null.
        let refusal_object = r#"{"choices":[{"message":{"content":null,"refusal":{"text":"x"}}}]}"#;
        assert_eq!(
            completion_cause(refusal_object),
            Some(Cause::NoContent(Missing::Refusal))
        );
        // The refusal comes first.
        let refused_and_cut =
            r#"{"choices":[{"message":{"refusal":"no"},"finish_reason":"length"}]}"#;
        assert_eq!(
            completion_cause(refused_and_cut),
            Some(Cause::NoContent(Missing::Refusal))
        );
        let filtered =
            r#"{"choices":[{"message":{"content":null},"finish_reason":"content_filter"}]}"#;
        assert_eq!(
            completion_cause(filtered),
            Some(Cause::NoContent(Missing::ContentFilter))
        );
        let cut =
            r#"{"choices":[{"message":{"content":null,"refusal":null},"finish_reason":"length"}]}"#;
        assert_eq!(
            completion_cause(cut),
            Some(Cause::NoContent(Missing::Length))
        );
        let no_message = r#"{"choices":[{"finish_reason":"length"}]}"#;
        assert_eq!(
            completion_cause(no_message),
            Some(Cause::NoContent(Missing::Length))
        );
        // A finish reason longer than 64 bytes is not read.
        let long_reason = format!(
            r#"{{"choices":[{{"message":{{"content":null}},"finish_reason":"length{}"}}]}}"#,
            "x".repeat(59)
        );
        assert_eq!(completion_cause(&long_reason), other);
    }

    #[test]
    fn a_string_content_is_an_answer_even_beside_a_refusal_a_cut_or_empty() {
        for (body, content) in [
            (
                r#"{"choices":[{"message":{"content":"{\"summary\":\"s\"}"}}]}"#,
                r#"{"summary":"s"}"#,
            ),
            (
                r#"{"choices":[{"message":{"content":"partial","refusal":"no"}}]}"#,
                "partial",
            ),
            (
                r#"{"choices":[{"message":{"content":"cut"},"finish_reason":"length"}]}"#,
                "cut",
            ),
            (
                r#"{"choices":[{"message":{"content":""},"finish_reason":"content_filter"}]}"#,
                "",
            ),
            (
                r#"{"choices":[{"message":{"content":"first"}},{"message":{"content":null}}]}"#,
                "first",
            ),
        ] {
            let answer = completion(body.as_bytes()).expect("an answer");
            assert_eq!(answer.content, content, "{body}");
            assert_eq!(answer.prompt_tokens, None, "{body}");
            assert_eq!(answer.completion_tokens, None, "{body}");
        }
    }

    #[test]
    fn the_usage_is_read_as_before() {
        let body = r#"{"choices":[{"message":{"content":"c"}}],"usage":{"prompt_tokens":120,"completion_tokens":30}}"#;
        let answer = completion(body.as_bytes()).expect("an answer");
        assert_eq!(
            (answer.prompt_tokens, answer.completion_tokens),
            (Some(120), Some(30))
        );
        for usage in [
            r#"{"prompt_tokens":-1,"completion_tokens":"30"}"#,
            r#"{"prompt_tokens":1.5}"#,
            "null",
            "[]",
        ] {
            let body =
                format!(r#"{{"choices":[{{"message":{{"content":"c"}}}}],"usage":{usage}}}"#);
            let answer = completion(body.as_bytes()).expect("an answer");
            assert_eq!(
                (answer.prompt_tokens, answer.completion_tokens),
                (None, None),
                "{usage}"
            );
        }
    }

    // `retry_after`: delta-seconds on 429 and 503 only, clamped to 30-600 s.

    fn seconds(n: u64) -> Option<Duration> {
        Some(Duration::from_secs(n))
    }

    #[test]
    fn retry_after_is_read_on_429_and_503_only_and_clamped() {
        assert_eq!(RETRY_AFTER_MIN, Duration::from_secs(30));
        assert_eq!(RETRY_AFTER_MAX, Duration::from_secs(600));
        let cases: &[(&str, Option<Duration>)] = &[
            ("0", seconds(30)),
            ("1", seconds(30)),
            ("29", seconds(30)),
            ("30", seconds(30)),
            ("45", seconds(45)),
            ("0045", seconds(45)),
            ("600", seconds(600)),
            ("601", seconds(600)),
            ("99999", seconds(600)),
            ("4294967296", seconds(600)),
            ("9999999999", seconds(600)),
            // 11 digits, a sign, white space, a fraction, a date or anything else: ignored.
            ("12345678901", None),
            ("-5", None),
            ("+5", None),
            (" 45", None),
            ("45 ", None),
            ("4 5", None),
            ("45.0", None),
            ("1e3", None),
            ("Wed, 21 Oct 2015 07:28:00 GMT", None),
            ("soon", None),
            ("", None),
            ("\u{0664}\u{0665}", None),
        ];
        for &(header, expected) in cases {
            for status in [429, 503] {
                assert_eq!(
                    retry_after(status, Some(header.as_bytes())),
                    expected,
                    "{status} '{header}'"
                );
            }
        }
        assert_eq!(retry_after(429, Some(b"\xff45")), None);
        for status in [429, 503] {
            assert_eq!(retry_after(status, None), None, "{status}");
        }
        for status in [200, 302, 400, 408, 413, 500, 502, 504] {
            assert_eq!(retry_after(status, Some(b"45")), None, "{status}");
        }
    }

    // The causes, their classes and their hints.

    const NAMES: [&str; 12] = [
        "connection",
        "timeout",
        "redirect",
        "not_an_answer",
        "no_content",
        "rate_limited",
        "quota",
        "key",
        "not_found",
        "too_large",
        "rejected",
        "server_error",
    ];

    #[test]
    fn every_cause_has_a_unique_name_a_class_and_a_hint() {
        assert_eq!(
            Cause::ALL,
            [
                Cause::Connection,
                Cause::Timeout,
                Cause::Redirect,
                Cause::NotAnAnswer,
                Cause::NoContent(Missing::Other),
                Cause::RateLimited,
                Cause::Quota,
                Cause::Key,
                Cause::NotFound,
                Cause::TooLarge,
                Cause::Rejected,
                Cause::ServerError,
            ]
        );
        let names: Vec<&str> = Cause::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(names, NAMES);
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "a name is used twice");
        for cause in Cause::ALL {
            for json_schema in [true, false] {
                assert!(!cause.hint(json_schema).is_empty(), "{cause:?}");
            }
        }
    }

    #[test]
    fn the_classes_are_those_of_the_adr() {
        use Class::{Configuration, Rejected, Transient};
        let classes: Vec<(Cause, Class)> = Cause::ALL.iter().map(|c| (*c, c.class())).collect();
        assert_eq!(
            classes,
            [
                (Cause::Connection, Transient),
                (Cause::Timeout, Transient),
                (Cause::Redirect, Configuration),
                (Cause::NotAnAnswer, Configuration),
                (Cause::NoContent(Missing::Other), Rejected),
                (Cause::RateLimited, Transient),
                (Cause::Quota, Configuration),
                (Cause::Key, Configuration),
                (Cause::NotFound, Configuration),
                (Cause::TooLarge, Rejected),
                (Cause::Rejected, Rejected),
                (Cause::ServerError, Transient),
            ]
        );
        assert_eq!(Transient.as_str(), "transient");
        assert_eq!(Configuration.as_str(), "configuration");
        assert_eq!(Rejected.as_str(), "rejected");
        // Every kind of missing content is one cause, `no_content`, neutral for the breaker.
        for kind in [
            Missing::Refusal,
            Missing::ContentFilter,
            Missing::Length,
            Missing::Other,
        ] {
            let cause = Cause::NoContent(kind);
            assert_eq!(cause.as_str(), "no_content", "{kind:?}");
            assert_eq!(cause.class(), Rejected, "{kind:?}");
            assert_eq!(
                cause.hint(true),
                Cause::NoContent(Missing::Other).hint(true),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn each_hint_is_its_fixed_sentence() {
        let hints: [(Cause, &str); 12] = [
            (
                Cause::Connection,
                "cannot connect: check model.endpoint (--endpoint), DNS, the certificate and \
                 networkPolicy.endpoint",
            ),
            (
                Cause::Timeout,
                "no answer within the timeout: the model may be overloaded or slow on this \
                 hardware; raise model.timeoutSeconds (--model-timeout) or try later",
            ),
            (
                Cause::Redirect,
                "the endpoint redirects and kausa follows no redirect: put the final URL in \
                 model.endpoint (--endpoint)",
            ),
            (
                Cause::NotAnAnswer,
                "the URL does not serve the OpenAI Chat Completions API: put the base URL (often \
                 ending in /v1) in model.endpoint (--endpoint); kausa appends /chat/completions",
            ),
            (
                Cause::NoContent(Missing::Other),
                "the model answered without text: it refused, a filter removed it, or it spent \
                 kausa's 800 output tokens before answering (reason says which); with the \
                 check's synthetic request, try another model, or one that does not reason at \
                 length",
            ),
            (
                Cause::RateLimited,
                "the provider limits the rate of this key: kausa waits and tries again",
            ),
            (
                Cause::Quota,
                "the provider's account has no quota or credit left: add credit, or use another \
                 key (model.apiKey.existingSecret, --api-key-file)",
            ),
            (
                Cause::Key,
                "the endpoint refused the API key: check the Secret in \
                 model.apiKey.existingSecret (--api-key-file) and the key's permissions for this \
                 model",
            ),
            (
                Cause::NotFound,
                "the model or the URL is wrong, or the model is no longer offered: check \
                 model.name (--model) and that model.endpoint is the base URL",
            ),
            (
                Cause::TooLarge,
                "the model refuses a request of this size: lower budget.maxInputTokens \
                 (--max-input-tokens) or use a model with a larger context",
            ),
            (
                Cause::Rejected,
                "the endpoint refused the request: if it does not support structured output, \
                 set model.jsonSchema=false (--no-json-schema); see detail",
            ),
            (
                Cause::ServerError,
                "the provider is failing: kausa holds calls and tries again later",
            ),
        ];
        for (cause, hint) in hints {
            assert_eq!(cause.hint(true), hint, "{cause:?}");
            if cause != Cause::Rejected {
                assert_eq!(cause.hint(false), hint, "{cause:?} without the JSON schema");
            }
        }
        // Without the JSON schema there is no response_format to blame.
        assert_eq!(
            Cause::Rejected.hint(false),
            "the endpoint refused the request: see detail"
        );
        assert_eq!(
            INVALID_ANSWER_HINT,
            "the model answered, but not with kausa's JSON: try another model, or \
             model.jsonSchema=false (--no-json-schema) if the server ignores response_format"
        );
    }

    /// The rows of the causes table in docs/reference/command-line.md: a first cell that is a
    /// name in backquotes and a cell that is a class, with its cells.
    fn documented_causes() -> Vec<(String, Vec<String>)> {
        include_str!("../../docs/reference/command-line.md")
            .lines()
            .filter_map(|line| {
                let cells: Vec<String> = line
                    .strip_prefix('|')?
                    .strip_suffix('|')?
                    .split('|')
                    .map(|cell| cell.trim().trim_matches('`').to_owned())
                    .collect();
                let name = line.strip_prefix("| `")?.split_once('`')?.0.to_owned();
                cells
                    .iter()
                    .skip(1)
                    .any(|cell| ["transient", "configuration", "rejected"].contains(&cell.as_str()))
                    .then_some((name, cells))
            })
            .collect()
    }

    #[test]
    fn the_causes_table_of_the_command_line_reference_matches_the_code() {
        let documented = documented_causes();
        let names: Vec<&str> = documented.iter().map(|(name, _)| name.as_str()).collect();
        // Every cause, once, in the order of `Cause::ALL`, and nothing else.
        assert_eq!(names, NAMES, "the causes table and Cause::ALL differ");
        for ((_, cells), cause) in documented.iter().zip(Cause::ALL) {
            let class = cause.class().as_str();
            let breaker = match cause.class() {
                Class::Transient => "counts",
                Class::Configuration => "opens",
                Class::Rejected => "none",
            };
            assert!(
                cells.iter().any(|cell| cell == class),
                "{}: not documented as {class}",
                cause.as_str()
            );
            assert!(
                cells.iter().any(|cell| cell == breaker),
                "{}: its breaker effect is not documented as {breaker}",
                cause.as_str()
            );
            let row = cells.join("|");
            for json_schema in [true, false] {
                assert!(
                    row.contains(cause.hint(json_schema)),
                    "{}: its hint is not in its row",
                    cause.as_str()
                );
            }
        }
    }
}
