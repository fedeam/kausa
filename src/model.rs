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
    pub async fn complete(&self, body: &Value) -> Result<Answer, String> {
        let mut request = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| format!("the request failed: {}", without_url(&e)))?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("reading the answer failed: {}", without_url(&e)))?
        {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "the answer is larger than {MAX_RESPONSE_BYTES} bytes"
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(format!("the endpoint answered {status}"));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("the answer is not JSON: {e}"))?;
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("the answer has no choices[0].message.content")?
            .to_owned();
        Ok(Answer {
            content,
            prompt_tokens: value["usage"]["prompt_tokens"].as_u64(),
            completion_tokens: value["usage"]["completion_tokens"].as_u64(),
        })
    }
}

/// A reqwest error without its URL, which may carry the base URL's user information.
fn without_url(error: &reqwest::Error) -> String {
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
        assert!(error.contains("307"), "{error}");
        server.join().expect("the server");
    }
}
