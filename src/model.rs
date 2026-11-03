//! The client of the one OpenAI-compatible endpoint (docs/adr/0008-data-leaving-the-cluster.md):
//! `POST <base URL>/chat/completions`, HTTPS unless plain HTTP is allowed explicitly, no redirects,
//! no proxy (reqwest would otherwise read `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` from the
//! environment), timeouts and a cap on the answer's size. Why a call failed is a cause kausa
//! defines, from the status and fixed keys of the answer (`pure`, docs/adr/0020-check-the-endpoint.md).

use std::time::Duration;

use serde_json::Value;

mod pure;

pub use pure::{Cause, Class, INVALID_ANSWER_HINT, Missing};

/// The largest answer kausa reads, in bytes.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// The time to connect, whatever the call's timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct ModelClient {
    http: reqwest::Client,
    url: String,
    api_key: Option<String>,
    timeout: Duration,
}

/// What the endpoint answered: its `2xx` status, the text of the first choice and the token
/// counts it reported.
pub struct Answer {
    pub status: u16,
    pub content: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

impl ModelClient {
    /// A client for `base_url` (for example `https://api.example.com/v1`) whose calls time out
    /// after `timeout` in all. Plain `http://` is refused unless `allow_http` is set.
    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        allow_http: bool,
        timeout: Duration,
    ) -> Result<Self, String> {
        let base = base_url.trim_end_matches('/');
        let scheme_ok = base.starts_with("https://") || (allow_http && base.starts_with("http://"));
        if !scheme_ok {
            return Err(if base.starts_with("http://") {
                "the endpoint uses plain HTTP: pass --allow-http for an endpoint inside the cluster \
                 or the same private network"
                    .into()
            } else {
                format!("the endpoint '{base}' is not an http(s) URL")
            });
        }
        // kausa uses ring, the provider the Kubernetes client is built with. Installing it twice
        // is harmless: the second call returns an error that is ignored.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!allow_http)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout)
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
        Ok(Self {
            http,
            url: format!("{base}/chat/completions"),
            api_key,
            timeout,
        })
    }

    /// A call's total timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Sends one request body and returns the answer, or why there is none.
    pub async fn complete(&self, body: &Value) -> Result<Answer, CallError> {
        let mut request = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let mut response = request.send().await.map_err(|e| {
            // A connect error first: the endpoint never received the request.
            let (cause, reached) = if e.is_connect() {
                (Cause::Connection, false)
            } else if e.is_timeout() {
                (Cause::Timeout, true)
            } else {
                (Cause::ServerError, true)
            };
            CallError {
                message: format!("the request failed: {}", without_url(&e)),
                detail: None,
                cause,
                status: None,
                retry_after: None,
                reached,
            }
        })?;
        let status = response.status();
        let code = status.as_u16();
        // Before the body, which may never come.
        let retry_after = pure::retry_after(
            code,
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .map(|v| v.as_bytes()),
        );
        let failed = |cause, message: String, detail| CallError {
            message,
            detail,
            cause,
            status: Some(code),
            retry_after,
            reached: true,
        };
        let mut bytes = Vec::new();
        let mut over_cap = false;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            // A body that stalls is a timeout; one cut short, a failing server.
            let cause = if e.is_timeout() {
                Cause::Timeout
            } else {
                Cause::ServerError
            };
            failed(
                cause,
                format!("reading the answer failed: {}", without_url(&e)),
                None,
            )
        })? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                let room = MAX_RESPONSE_BYTES - bytes.len();
                bytes.extend_from_slice(&chunk[..room]);
                over_cap = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            // An error body over the cap is classified by its status alone.
            let cause = pure::classify(code, if over_cap { b"" } else { &bytes });
            return Err(failed(
                cause,
                format!("the endpoint answered {status}"),
                Some(error_detail(&bytes)),
            ));
        }
        // A 2xx keeps no detail: its text is the model's (docs/adr/0022-redact-the-answer.md).
        if over_cap {
            return Err(failed(
                Cause::NotAnAnswer,
                format!("the answer is larger than {MAX_RESPONSE_BYTES} bytes"),
                None,
            ));
        }
        match pure::completion(&bytes) {
            Ok(completion) => Ok(Answer {
                status: code,
                content: completion.content,
                prompt_tokens: completion.prompt_tokens,
                completion_tokens: completion.completion_tokens,
            }),
            Err(cause) => Err(failed(cause, no_answer_reason(cause, &bytes), None)),
        }
    }
}

/// The fixed reason of a `2xx` that is not a completion with content: never the model's text.
fn no_answer_reason(cause: Cause, body: &[u8]) -> String {
    match cause {
        Cause::NoContent(Missing::Refusal) => {
            "the model answered with no content: it refused".into()
        }
        Cause::NoContent(Missing::ContentFilter) => {
            "the model answered with no content: a content filter removed it".into()
        }
        Cause::NoContent(Missing::Length) => {
            "the model answered with no content: it ran out of output tokens".into()
        }
        Cause::NoContent(Missing::Other) => "the model answered with no content".into(),
        _ => match serde_json::from_slice::<Value>(body) {
            Err(e) => format!("the answer is not JSON: {e}"),
            Ok(_) => "the answer is not a chat completion: it has no choices array".into(),
        },
    }
}

/// Why a call failed.
#[derive(Debug, PartialEq, Eq)]
pub struct CallError {
    /// What went wrong, for the log and the `ExplanationSkipped` Event: kausa's text, fixed for an
    /// answer with no content.
    pub message: String,
    /// The start of the endpoint's error body, for kausa's log only: it is the provider's text,
    /// so it does not go into an Event. `None` for a `2xx`, whose text is the model's.
    pub detail: Option<String>,
    /// Why, as kausa names it; its class says whether to retry and what the breaker does.
    pub cause: Cause,
    /// The status of the answer; `None` when there was none (a failed connection, a timeout).
    pub status: Option<u16>,
    /// The delay a `Retry-After` on a `429` or `503` asked for, clamped to 30-600 s.
    pub retry_after: Option<Duration>,
    /// Whether the endpoint received the request (and may have counted it).
    pub reached: bool,
}

/// The longest part of an error body kept for the log.
const MAX_ERROR_DETAIL: usize = 512;

/// The start of an error body as one line of text: invalid UTF-8 replaced, control characters
/// and runs of white space turned into single spaces, cut at a character boundary.
fn error_detail(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let mut line = String::new();
    for c in text.chars() {
        let c = if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        };
        if c == ' ' && (line.is_empty() || line.ends_with(' ')) {
            continue;
        }
        if line.len() + c.len_utf8() > MAX_ERROR_DETAIL {
            break;
        }
        line.push(c);
    }
    line.trim_end().to_owned()
}

/// A reqwest error without its URL, which may carry the base URL's user information.
pub(crate) fn without_url(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    if let Some(url) = error.url() {
        text = text.replace(url.as_str(), "<endpoint>");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // Acceptance tests for docs/adr/0020-check-the-endpoint.md, written before the code, beside
    // the tests of v0.4. They use `ModelClient::new(base_url, api_key, allow_http, timeout:
    // Duration)`, `ModelClient::timeout(&self) -> Duration`, `CallError { message, detail, cause:
    // Cause, status: Option<u16>, retry_after: Option<Duration>, reached }` (no `transient`),
    // `mod pure` with the API its own tests list, `prompt::MAX_TOKENS` and
    // `breaker::MAX_HOLD`.

    use super::pure::{Cause, Missing};

    /// The default of `--model-timeout`.
    const TIMEOUT: Duration = Duration::from_secs(120);

    /// A one-shot HTTP server on localhost that answers `response` and returns the request it
    /// received.
    fn serve_once(response: String) -> (String, std::thread::JoinHandle<String>) {
        serve_once_bytes(response.into_bytes())
    }

    fn serve_once_bytes(response: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}/v1", listener.local_addr().expect("an address"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a connection");
            let mut buffer = vec![0; 65536];
            let mut request = Vec::new();
            loop {
                let n = stream.read(&mut buffer).expect("a read");
                request.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let length = text[..head_end]
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= head_end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            // The client may close first (an answer over the cap).
            let _ = stream.write_all(&response);
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, handle)
    }

    fn http_response(status: &str, body: &str) -> String {
        http_response_with(status, "", body)
    }

    /// An answer with `headers` (each ending in `\r\n`) beside the usual ones.
    fn http_response_with(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(f)
    }

    /// The error of one call to a server that answers `response` once.
    fn call_error(response: String) -> CallError {
        let (url, server) = serve_once(response);
        let client = ModelClient::new(&url, None, true, TIMEOUT).expect("a client");
        let error = block_on(client.complete(&serde_json::json!({})))
            .err()
            .expect("an error");
        server.join().expect("the server");
        error
    }

    #[test]
    fn plain_http_needs_the_flag() {
        assert!(ModelClient::new("http://127.0.0.1:11434/v1", None, false, TIMEOUT).is_err());
        assert!(ModelClient::new("http://127.0.0.1:11434/v1", None, true, TIMEOUT).is_ok());
        assert!(ModelClient::new("ftp://example.com", None, true, TIMEOUT).is_err());
    }

    #[test]
    fn the_client_keeps_the_timeout_it_was_given() {
        for seconds in [1, 10, 120, 600] {
            let client = ModelClient::new(
                "https://api.example.com/v1",
                None,
                false,
                Duration::from_secs(seconds),
            )
            .expect("a client");
            assert_eq!(client.timeout(), Duration::from_secs(seconds));
        }
    }

    #[test]
    fn a_completion_round_trip() {
        let body = r#"{"choices":[{"message":{"content":"{\"summary\":\"s\"}"}}],"usage":{"prompt_tokens":120,"completion_tokens":30}}"#;
        let (url, server) = serve_once(http_response("200 OK", body));
        let client =
            ModelClient::new(&url, Some("not-a-real-key".into()), true, TIMEOUT).expect("a client");
        let answer =
            block_on(client.complete(&serde_json::json!({"model": "m"}))).expect("an answer");
        assert_eq!(answer.content, r#"{"summary":"s"}"#);
        assert_eq!(answer.prompt_tokens, Some(120));
        assert_eq!(answer.completion_tokens, Some(30));
        let request = server.join().expect("the server");
        assert!(
            request.starts_with("POST /v1/chat/completions "),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer not-a-real-key")
        );
    }

    #[test]
    fn redirects_are_not_followed_and_errors_are_reported() {
        let error = call_error(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://192.0.2.1/steal\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
        );
        assert!(error.message.contains("307"), "{error:?}");
        assert_eq!(error.cause, Cause::Redirect, "{error:?}");
        assert_eq!(error.status, Some(307));
        assert!(error.reached);
    }

    #[test]
    fn an_error_status_keeps_the_start_of_its_body_and_says_whether_to_retry() {
        let body = format!("{{\"error\":\"rate\nlimited\"}}{}", "x".repeat(2000));
        let error = call_error(http_response("429 Too Many Requests", &body));
        assert_eq!(error.message, "the endpoint answered 429 Too Many Requests");
        let detail = error.detail.as_deref().expect("a detail");
        assert!(
            detail.starts_with("{\"error\":\"rate limited\"}"),
            "{detail}"
        );
        assert!(detail.len() <= MAX_ERROR_DETAIL);
        assert_eq!(error.cause, Cause::RateLimited, "{error:?}");
        assert_eq!((error.status, error.retry_after), (Some(429), None));
        assert!(error.reached);

        let error = call_error(http_response("400 Bad Request", "{}"));
        assert_eq!(error.cause, Cause::Rejected, "{error:?}");
        assert_eq!(error.status, Some(400));
    }

    #[test]
    fn a_refused_connection_never_reached_the_endpoint() {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let client = ModelClient::new(&format!("http://127.0.0.1:{port}/v1"), None, true, TIMEOUT)
            .expect("a client");
        let error = block_on(client.complete(&serde_json::json!({})))
            .err()
            .expect("an error");
        assert_eq!(error.cause, Cause::Connection, "{error:?}");
        assert!(!error.reached, "{error:?}");
        assert_eq!((error.status, error.retry_after), (None, None));
        assert_eq!(error.detail, None);
    }

    #[test]
    fn a_server_that_never_answers_times_out_after_the_timeout() {
        // Connected (the kernel accepts it), never answered.
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}/v1", listener.local_addr().expect("an address"));
        let client = ModelClient::new(&url, None, true, Duration::from_secs(1)).expect("a client");
        let start = std::time::Instant::now();
        let error = block_on(client.complete(&serde_json::json!({})))
            .err()
            .expect("an error");
        let took = start.elapsed();
        assert_eq!(error.cause, Cause::Timeout, "{error:?}");
        assert!(error.reached, "{error:?}");
        assert_eq!(error.status, None);
        assert!(
            took >= Duration::from_millis(900) && took < Duration::from_secs(5),
            "{took:?}"
        );
        drop(listener);
    }

    #[test]
    fn each_status_has_its_cause_status_and_retry_after() {
        let quota = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;
        let too_long = r#"{"error":{"message":"too long","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        let cases: Vec<(String, Cause, u16, Option<Duration>)> = vec![
            (
                http_response("401 Unauthorized", r#"{"error":{"message":"bad key"}}"#),
                Cause::Key,
                401,
                None,
            ),
            (http_response("403 Forbidden", "{}"), Cause::Key, 403, None),
            (
                http_response("404 Not Found", r#"{"error":{"code":"model_not_found"}}"#),
                Cause::NotFound,
                404,
                None,
            ),
            (
                http_response("429 Too Many Requests", quota),
                Cause::Quota,
                429,
                None,
            ),
            (
                http_response_with("429 Too Many Requests", "retry-after: 120\r\n", "{}"),
                Cause::RateLimited,
                429,
                Some(Duration::from_secs(120)),
            ),
            (
                http_response_with("503 Service Unavailable", "retry-after: 99999\r\n", ""),
                Cause::ServerError,
                503,
                Some(Duration::from_secs(600)),
            ),
            (
                http_response_with("503 Service Unavailable", "retry-after: soon\r\n", ""),
                Cause::ServerError,
                503,
                None,
            ),
            (
                // A quota answer may say when to come back; it is still never retried.
                http_response_with("429 Too Many Requests", "retry-after: 45\r\n", quota),
                Cause::Quota,
                429,
                Some(Duration::from_secs(45)),
            ),
            (
                http_response("413 Payload Too Large", ""),
                Cause::TooLarge,
                413,
                None,
            ),
            (
                http_response("400 Bad Request", too_long),
                Cause::TooLarge,
                400,
                None,
            ),
            (
                http_response("402 Payment Required", "{}"),
                Cause::Quota,
                402,
                None,
            ),
            (
                http_response("408 Request Timeout", ""),
                Cause::Timeout,
                408,
                None,
            ),
            (
                http_response("500 Internal Server Error", "oops"),
                Cause::ServerError,
                500,
                None,
            ),
        ];
        for (response, cause, status, retry_after) in cases {
            let error = call_error(response);
            assert_eq!(error.cause, cause, "{status}: {error:?}");
            assert_eq!(error.status, Some(status), "{error:?}");
            assert_eq!(error.retry_after, retry_after, "{status}: {error:?}");
            assert!(error.reached, "{error:?}");
            let reason = error.status.map(|s| format!("the endpoint answered {s} "));
            assert!(
                error
                    .message
                    .starts_with(reason.as_deref().unwrap_or_default()),
                "{error:?}"
            );
            assert!(error.detail.is_some(), "{status}: no detail");
        }
    }

    #[test]
    fn a_2xx_that_is_not_a_chat_completion_opens_with_its_reason() {
        let error = call_error(
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nOK"
                .into(),
        );
        assert_eq!(error.cause, Cause::NotAnAnswer, "{error:?}");
        assert_eq!(error.status, Some(200));
        assert!(
            error.message.starts_with("the answer is not JSON: "),
            "{error:?}"
        );
        assert_eq!(error.detail, None, "a 2xx keeps no detail");

        let error = call_error(http_response("200 OK", r#"{"object":"list","data":[]}"#));
        assert_eq!(error.cause, Cause::NotAnAnswer, "{error:?}");
        assert_eq!(
            error.message,
            "the answer is not a chat completion: it has no choices array"
        );
        assert_eq!((error.status, error.detail), (Some(200), None));
    }

    #[test]
    fn a_2xx_over_the_cap_is_not_an_answer() {
        let body = format!(
            r#"{{"choices":[{{"message":{{"content":"{}"}}}}]}}"#,
            "x".repeat(MAX_RESPONSE_BYTES)
        );
        let error = call_error(http_response("200 OK", &body));
        assert_eq!(error.cause, Cause::NotAnAnswer, "{:?}", error.cause);
        assert_eq!(error.message, "the answer is larger than 262144 bytes");
        assert_eq!((error.status, error.detail), (Some(200), None));
        assert!(error.reached);
    }

    #[test]
    fn a_non_2xx_over_the_cap_is_classified_by_its_status() {
        let body = format!(
            r#"{{"error":{{"code":"insufficient_quota","message":"{}"}}}}"#,
            "x".repeat(MAX_RESPONSE_BYTES)
        );
        let error = call_error(http_response("429 Too Many Requests", &body));
        // An error body over the cap is not parsed: the status alone, though the code comes first.
        assert_eq!(error.cause, Cause::RateLimited, "{:?}", error.cause);
        assert_eq!(error.status, Some(429));
        assert!(error.reached);
    }

    #[test]
    fn a_chat_completion_with_no_content_names_its_kind_and_never_its_text() {
        // Invented bodies in the shapes ADR 0020 expects (hypotheses until a body is captured).
        const CANARY: &str = "REFUSAL-CANARY-text";
        let cases = [
            (
                format!(
                    r#"{{"choices":[{{"message":{{"role":"assistant","content":null,"refusal":"{CANARY}"}},"finish_reason":"stop"}}]}}"#
                ),
                Missing::Refusal,
                "the model answered with no content: it refused",
            ),
            (
                format!(
                    r#"{{"choices":[{{"message":{{"role":"assistant","content":null}},"finish_reason":"content_filter","note":"{CANARY}"}}]}}"#
                ),
                Missing::ContentFilter,
                "the model answered with no content: a content filter removed it",
            ),
            (
                format!(
                    r#"{{"choices":[{{"message":{{"role":"assistant","content":null,"reasoning":"{CANARY}"}},"finish_reason":"length"}}]}}"#
                ),
                Missing::Length,
                "the model answered with no content: it ran out of output tokens",
            ),
            (
                r#"{"choices":[{"message":{"role":"assistant"}}]}"#.to_owned(),
                Missing::Other,
                "the model answered with no content",
            ),
        ];
        for (body, kind, message) in cases {
            let error = call_error(http_response("200 OK", &body));
            assert_eq!(error.cause, Cause::NoContent(kind), "{kind:?}");
            assert_eq!(error.message, message, "{kind:?}");
            assert_eq!(error.detail, None, "{kind:?}: a 2xx keeps no detail");
            assert_eq!(error.status, Some(200), "{kind:?}");
            assert_eq!(error.retry_after, None, "{kind:?}");
            assert!(error.reached, "{kind:?}");
            assert!(
                !format!("{error:?}").contains(CANARY),
                "{kind:?}: the model's text is in the error"
            );
        }
    }

    #[test]
    fn a_body_cut_short_is_a_server_error_that_reached_the_endpoint() {
        // The answer announces more bytes than it sends, then closes.
        let error = call_error(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1000\r\nconnection: close\r\n\r\n{\"choices\":"
                .into(),
        );
        assert_eq!(error.cause, Cause::ServerError, "{error:?}");
        assert!(error.reached);
    }

    #[test]
    fn the_no_content_hint_names_the_output_tokens_kausa_asks_for() {
        assert_eq!(crate::prompt::MAX_TOKENS, 800);
        let hint = Cause::NoContent(Missing::Length).hint(true);
        assert!(
            hint.contains(&format!(
                "kausa's {} output tokens",
                crate::prompt::MAX_TOKENS
            )),
            "{hint}"
        );
    }

    #[test]
    fn retry_after_never_holds_longer_than_the_breaker() {
        assert!(super::pure::RETRY_AFTER_MAX <= crate::breaker::MAX_HOLD);
    }

    #[test]
    fn error_details_are_one_short_line() {
        assert_eq!(error_detail(b"  a\r\n\tb\x00c  "), "a b c");
        let long = "é".repeat(400);
        let detail = error_detail(long.as_bytes());
        assert!(detail.len() <= MAX_ERROR_DETAIL && detail.chars().all(|c| c == 'é'));
    }
}
