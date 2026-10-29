//! The client of the one OpenAI-compatible endpoint (docs/adr/0008-data-leaving-the-cluster.md):
//! `POST <base URL>/chat/completions`, HTTPS unless plain HTTP is allowed explicitly, no redirects,
//! no proxy (reqwest would otherwise read `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` from the
//! environment), timeouts and a cap on the answer's size.

use std::time::Duration;

use serde_json::Value;

/// The largest answer kausa reads, in bytes.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub struct ModelClient {
    http: reqwest::Client,
    url: String,
    api_key: Option<String>,
}

/// What the endpoint answered: the text of the first choice and the token counts it reported.
pub struct Answer {
    pub content: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

impl ModelClient {
    /// A client for `base_url` (for example `https://api.example.com/v1`). Plain `http://` is
    /// refused unless `allow_http` is set.
    pub fn new(base_url: &str, api_key: Option<String>, allow_http: bool) -> Result<Self, String> {
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
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
        Ok(Self {
            http,
            url: format!("{base}/chat/completions"),
            api_key,
        })
    }

    /// Sends one request body and returns the answer.
    pub async fn complete(&self, body: &Value) -> Result<Answer, CallError> {
        let mut request = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let mut response = request.send().await.map_err(|e| CallError {
            message: format!("the request failed: {}", without_url(&e)),
            detail: None,
            transient: e.is_timeout() || e.is_connect(),
            // A connect error means the endpoint never received the request.
            reached: !e.is_connect(),
        })?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| CallError {
            message: format!("reading the answer failed: {}", without_url(&e)),
            detail: None,
            transient: e.is_timeout(),
            reached: true,
        })? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(CallError::permanent(format!(
                    "the answer is larger than {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(CallError {
                message: format!("the endpoint answered {status}"),
                detail: Some(error_detail(&bytes)),
                transient: status.as_u16() == 429 || status.is_server_error(),
                reached: true,
            });
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| CallError::permanent(format!("the answer is not JSON: {e}")))?;
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| CallError::permanent("the answer has no choices[0].message.content"))?
            .to_owned();
        Ok(Answer {
            content,
            prompt_tokens: value["usage"]["prompt_tokens"].as_u64(),
            completion_tokens: value["usage"]["completion_tokens"].as_u64(),
        })
    }
}

/// Why a call failed.
#[derive(Debug, PartialEq, Eq)]
pub struct CallError {
    /// What went wrong, for the log and the `ExplanationSkipped` Event.
    pub message: String,
    /// The start of the endpoint's error body, for kausa's log only: it is the provider's text,
    /// so it does not go into an Event.
    pub detail: Option<String>,
    /// Worth one more try: a timeout, a failed connection, `429` or a `5xx` status.
    pub transient: bool,
    /// Whether the endpoint received the request (and may have counted it).
    pub reached: bool,
}

impl CallError {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: None,
            transient: false,
            reached: true,
        }
    }
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

    /// A one-shot HTTP server on localhost that answers `response` and returns the request it
    /// received.
    fn serve_once(response: String) -> (String, std::thread::JoinHandle<String>) {
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
            stream.write_all(response.as_bytes()).expect("a write");
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, handle)
    }

    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

    #[test]
    fn plain_http_needs_the_flag() {
        assert!(ModelClient::new("http://127.0.0.1:11434/v1", None, false).is_err());
        assert!(ModelClient::new("http://127.0.0.1:11434/v1", None, true).is_ok());
        assert!(ModelClient::new("ftp://example.com", None, true).is_err());
    }

    #[test]
    fn a_completion_round_trip() {
        let body = r#"{"choices":[{"message":{"content":"{\"summary\":\"s\"}"}}],"usage":{"prompt_tokens":120,"completion_tokens":30}}"#;
        let (url, server) = serve_once(http_response("200 OK", body));
        let client = ModelClient::new(&url, Some("not-a-real-key".into()), true).expect("a client");
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
        let (url, server) = serve_once(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://192.0.2.1/steal\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
        );
        let client = ModelClient::new(&url, None, true).expect("a client");
        let error = block_on(client.complete(&serde_json::json!({})))
            .err()
            .expect("an error");
        assert!(error.message.contains("307"), "{error:?}");
        assert!(!error.transient);
        server.join().expect("the server");
    }

    #[test]
    fn an_error_status_keeps_the_start_of_its_body_and_says_whether_to_retry() {
        let body = format!("{{\"error\":\"rate\nlimited\"}}{}", "x".repeat(2000));
        let (url, server) = serve_once(http_response("429 Too Many Requests", &body));
        let client = ModelClient::new(&url, None, true).expect("a client");
        let error = block_on(client.complete(&serde_json::json!({})))
            .err()
            .expect("an error");
        assert_eq!(error.message, "the endpoint answered 429 Too Many Requests");
        let detail = error.detail.expect("a detail");
        assert!(
            detail.starts_with("{\"error\":\"rate limited\"}"),
            "{detail}"
        );
        assert!(detail.len() <= MAX_ERROR_DETAIL);
        assert!(error.transient && error.reached);
        server.join().expect("the server");

        let (url, server) = serve_once(http_response("400 Bad Request", "{}"));
        let client = ModelClient::new(&url, None, true).expect("a client");
        let error = block_on(client.complete(&serde_json::json!({})))
            .err()
            .expect("an error");
        assert!(!error.transient);
        server.join().expect("the server");
    }

    #[test]
    fn a_refused_connection_is_transient_and_never_reached_the_endpoint() {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let client =
            ModelClient::new(&format!("http://127.0.0.1:{port}/v1"), None, true).expect("a client");
        let error = block_on(client.complete(&serde_json::json!({})))
            .err()
            .expect("an error");
        assert!(error.transient && !error.reached, "{error:?}");
    }

    #[test]
    fn error_details_are_one_short_line() {
        assert_eq!(error_detail(b"  a\r\n\tb\x00c  "), "a b c");
        let long = "é".repeat(400);
        let detail = error_detail(long.as_bytes());
        assert!(detail.len() <= MAX_ERROR_DETAIL && detail.chars().all(|c| c == 'é'));
    }
}
