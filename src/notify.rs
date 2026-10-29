//! Notifications (docs/adr/0015-notifications.md): the webhook's URL check, its client, the queue
//! of notifications and their delivery, one at a time, outside the explanation path. The bodies
//! are built by `notification.rs`.
//!
//! The webhook URL and the bearer token are secrets: they are never logged, and no error, reason
//! or `Debug` output carries them. Only the status code of an answer is read, never its body.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use reqwest::Url;
use serde_json::json;
use tokio::time::Instant;

use crate::budget::Dedup;
use crate::explain::{self, Shared};
use crate::incident::Incident;
use crate::log;
use crate::model::without_url;
use crate::notification::{self, ChatId, Notice};
use crate::prompt::Explanation;

/// How many notifications may wait for the webhook; the oldest is dropped when one more arrives.
pub const QUEUE_LIMIT: usize = 16;
/// The least time between the starts of two sends, so that one burst stays within the rate a chat
/// service takes: one message per second for Slack's incoming webhooks, and for `json`.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// The same for `telegram`: Telegram lets a bot send 20 messages a minute to a group.
pub const TELEGRAM_MIN_INTERVAL: Duration = Duration::from_secs(3);

/// The time to connect, and the time for a whole request, before it fails as transient.
#[cfg(not(test))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const TIMEOUT: Duration = Duration::from_secs(3);

/// The wait before the one retry of a transient failure: `explain::RETRY_DELAY`, which the log
/// line states, and a short one in the tests.
#[cfg(not(test))]
const RETRY_WAIT: Duration = explain::RETRY_DELAY;
#[cfg(test)]
const RETRY_WAIT: Duration = Duration::from_millis(100);

const USER_AGENT: &str = concat!("kausa/", env!("CARGO_PKG_VERSION"));

/// The body the webhook takes (`--notify-format`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Slack,
    /// Telegram's `sendMessage`, to this chat.
    Telegram(ChatId),
}

impl Format {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Slack => "slack",
            Self::Telegram(_) => "telegram",
        }
    }

    /// The least time between the starts of two sends in this format.
    pub fn min_interval(&self) -> Duration {
        match self {
            Self::Json | Self::Slack => MIN_INTERVAL,
            Self::Telegram(_) => TELEGRAM_MIN_INTERVAL,
        }
    }
}

/// Checks the webhook URL read from `--notify-url-file`: it parses, it is `https://` (or `http://`
/// with `allow_http`), it carries no user name or password, and for Telegram its path is exactly
/// `/bot<token>/sendMessage` on any host (the query is not checked). The error is the end of the
/// usage error, after "the URL in --notify-url-file '<path>' ", and never carries the URL.
pub fn check_url(text: &str, format: &Format, allow_http: bool) -> Result<Url, &'static str> {
    let url = Url::parse(text).map_err(|_| "is not a valid URL")?;
    let scheme_ok = url.scheme() == "https" || (allow_http && url.scheme() == "http");
    if !scheme_ok {
        return Err("must be https:// (or http:// with --notify-allow-http)");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not carry a user name or password");
    }
    if matches!(format, Format::Telegram(_)) && !is_send_message(url.path()) {
        return Err("must end in /bot<token>/sendMessage with --notify-format telegram");
    }
    Ok(url)
}

/// Whether `path` is `/bot<token>/sendMessage`, with a token that is not empty and has no `/`.
fn is_send_message(path: &str) -> bool {
    path.strip_prefix("/bot")
        .and_then(|rest| rest.strip_suffix("/sendMessage"))
        .is_some_and(|token| !token.is_empty() && !token.contains('/'))
}

/// The client of the one webhook. It has no `Debug`: the URL and the token are secrets.
pub struct Notifier {
    client: reqwest::Client,
    url: Url,
    format: Format,
    /// The bearer token, kept only for the `json` format.
    token: Option<String>,
}

/// Why one attempt failed. `reason` is for the log: it never carries the URL or the token, nor
/// anything the webhook answered but its status code.
#[derive(Debug)]
pub struct Failure {
    pub reason: String,
    /// Worth one more try: a timeout, a failed connection, `429` or a `5xx` status.
    pub transient: bool,
}

impl Notifier {
    /// A client for `url`, checked by `check_url`: rustls, no proxy, no redirects, `https_only`
    /// unless `allow_http`, a 5 s connect and a 10 s total timeout. `token` is used only with the
    /// `json` format. The error is the whole usage error and carries no URL.
    pub fn new(
        url: Url,
        format: Format,
        token: Option<String>,
        allow_http: bool,
    ) -> Result<Self, String> {
        // The provider the Kubernetes client and the model's client use; a second install is
        // refused, and that error is ignored.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!allow_http)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| format!("cannot build the webhook client: {}", without_url(&e)))?;
        let token = token.filter(|_| format == Format::Json);
        Ok(Self {
            client,
            url,
            format,
            token,
        })
    }

    /// The webhook's host, without its port, path or query: what the `started` line shows.
    pub fn host(&self) -> &str {
        self.url.host_str().unwrap_or_default()
    }

    pub fn format(&self) -> &Format {
        &self.format
    }

    /// One attempt: a `POST` of the body of `notice` in this format. Returns the status code of
    /// a `2xx`; a redirect is not followed, and the answer's body is never read.
    pub async fn send(&self, notice: &Notice) -> Result<u16, Failure> {
        let body = match &self.format {
            Format::Json => notification::json_body(notice, env!("CARGO_PKG_VERSION")),
            Format::Slack => notification::slack_body(notice),
            Format::Telegram(chat) => notification::telegram_body(notice, chat),
        };
        let mut request = self
            .client
            .post(self.url.clone())
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| Failure {
            reason: format!("the request failed: {}", without_url(&e)),
            transient: e.is_timeout() || e.is_connect(),
        })?;
        let status = response.status();
        drop(response);
        if status.is_success() {
            return Ok(status.as_u16());
        }
        if status.is_redirection() {
            return Err(Failure {
                reason: "a redirect is not followed: put the final URL in the file".into(),
                transient: false,
            });
        }
        Err(Failure {
            reason: format!("the webhook answered {}", status.as_u16()),
            transient: status.as_u16() == 429 || status.is_server_error(),
        })
    }
}

/// One notification waiting for the webhook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    /// The incident's fingerprint, the key of its `Dedup` entry.
    pub fingerprint: String,
    /// The explanation as the notification restores it (only the namespace and the top-level
    /// owner), which the `Dedup` entry stores as its `notice`.
    pub explanation: Explanation,
    pub notice: Notice,
}

/// The notification of `incident` with `narrow`, the explanation restored by
/// `Redactor::restore_only` for the namespace and the top-level owner.
pub fn queued(incident: &Incident, narrow: Explanation) -> Queued {
    let detection = incident.key.detection;
    let notice = Notice {
        detection_id: detection.id().to_owned(),
        detection_name: detection.name().to_owned(),
        api_version: incident.owner.api_version.clone(),
        kind: incident.owner.kind.clone(),
        namespace: incident.key.namespace.clone(),
        name: incident.owner.name.clone(),
        fingerprint: incident.fingerprint.clone(),
        // The same string as the incident's log line.
        first_seen: incident.first_seen.to_string(),
        summary: narrow.summary.clone(),
        cause: narrow.cause.clone(),
        steps: narrow.steps.clone(),
        confidence: narrow.confidence.clone(),
    };
    Queued {
        fingerprint: incident.fingerprint.clone(),
        explanation: narrow,
        notice,
    }
}

/// Queues `item`; when the queue is full, its oldest notification is dropped, counted and logged.
pub fn enqueue(shared: &Shared<'_>, item: Queued) -> io::Result<()> {
    let dropped = {
        let mut queue = shared.notifications.borrow_mut();
        let dropped = if queue.len() >= QUEUE_LIMIT {
            queue.pop_front()
        } else {
            None
        };
        queue.push_back(item);
        dropped
    };
    if let Some(dropped) = dropped {
        shared
            .metrics
            .inc("kausa_notifications_total", &[("outcome", "dropped")]);
        log::warn(
            "notification dropped",
            json!({"fingerprint": dropped.fingerprint, "reason": "the queue is full"}),
        )?;
    }
    Ok(())
}

/// The dequeue rule: pops and drops, without counting or logging them, the notifications whose
/// `Dedup` entry (expired or not) stores the same text and is notified, and returns the first
/// one that is not. An evicted or replaced entry does not stop a notification.
pub fn take_next(queue: &mut VecDeque<Queued>, dedup: &Dedup) -> Option<Queued> {
    while let Some(item) = queue.pop_front() {
        if !dedup.already_sent(&item.fingerprint, &item.explanation) {
            return Some(item);
        }
    }
    None
}

/// Sends one notification: one attempt and, after a transient failure, one more after
/// `RETRY_DELAY`. Counts and logs the outcome, marks the `Dedup` entry notified after a success
/// (only if it still stores this text), then waits until the format's least interval
/// ([`Format::min_interval`]) has passed since the last attempt began. A failure is only logged and counted; the error is the log's.
pub async fn deliver(notifier: &Notifier, item: Queued, shared: &Shared<'_>) -> io::Result<()> {
    let mut attempt: u8 = 1;
    loop {
        let started = Instant::now();
        let result = notifier.send(&item.notice).await;
        match result {
            Ok(status) => {
                shared
                    .metrics
                    .inc("kausa_notifications_total", &[("outcome", "sent")]);
                shared
                    .dedup
                    .borrow_mut()
                    .mark_notified(&item.fingerprint, &item.explanation);
                log::info(
                    "notification sent",
                    json!({
                        "fingerprint": item.fingerprint,
                        "namespace": item.notice.namespace,
                        "owner": {"kind": item.notice.kind, "name": item.notice.name},
                        "format": notifier.format().name(),
                        "status": status,
                        "attempt": attempt,
                    }),
                )?;
            }
            Err(failure) if failure.transient && attempt == 1 => {
                shared.metrics.inc("kausa_notifications_retried_total", &[]);
                log::warn(
                    "notification retried",
                    json!({
                        "fingerprint": item.fingerprint,
                        "reason": failure.reason,
                        "retry_in_seconds": explain::RETRY_DELAY.as_secs(),
                    }),
                )?;
                tokio::time::sleep(RETRY_WAIT).await;
                attempt = 2;
                continue;
            }
            Err(failure) => {
                shared
                    .metrics
                    .inc("kausa_notifications_total", &[("outcome", "failed")]);
                log::warn(
                    "notification failed",
                    json!({"fingerprint": item.fingerprint, "reason": failure.reason}),
                )?;
            }
        }
        tokio::time::sleep_until(started + notifier.format().min_interval()).await;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{BTreeSet, VecDeque};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use k8s_openapi::jiff::Timestamp;
    use reqwest::Url;
    use serde_json::Value;

    use crate::budget::{Budget, Dedup};
    use crate::context::{self, Gathered};
    use crate::detect::Detection;
    use crate::explain::Shared;
    use crate::incident::{Incident, IncidentKey, Signal};
    use crate::metrics::Metrics;
    use crate::notification::{self, ChatId, Notice, parse_chat_id};
    use crate::prompt::{self, Explanation};
    use crate::record::{ContainerRecord, EventRecord, OwnerRef, PodRecord, Regarding};

    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const FINGERPRINT: &str = "5d0c2b7e9a1f4c38";

    /// A visibly fake token in the shape of a Telegram bot token, built at run time so that no
    /// secret scanner takes the source for a real one.
    fn fake_telegram_token() -> String {
        format!("{}:{}{}", "0".repeat(10), "AA", "x".repeat(33))
    }

    fn fake_bearer() -> String {
        format!("kausa-test-{}", "bearer")
    }

    fn incident() -> Incident {
        let owner = OwnerRef {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "web".into(),
            uid: "an-invented-uid".into(),
        };
        let key = IncidentKey {
            namespace: "shop".into(),
            owner_kind: "Deployment".into(),
            owner_name: "web".into(),
            detection: Detection::CrashLoop,
        };
        let first_seen: Timestamp = "2026-10-28T11:59:00Z".parse().expect("a timestamp");
        Incident {
            sample: Signal {
                key: key.clone(),
                owner: owner.clone(),
                observed_at: first_seen,
                event: None,
                pod: Some("web-7d9c8b6f4d-2xkqp".into()),
                container: Some("server".into()),
                image: None,
                reason: "CrashLoopBackOff".into(),
                message: "SAMPLE-CANARY back-off restarting failed container".into(),
                exit_code: Some(1),
            },
            key,
            owner,
            fingerprint: FINGERPRINT.into(),
            first_seen,
            last_seen: first_seen,
            pods: BTreeSet::from(["web-7d9c8b6f4d-2xkqp".to_owned()]),
        }
    }

    fn explanation(summary: &str) -> Explanation {
        Explanation {
            summary: summary.into(),
            cause: "It is started with a flag it does not know.".into(),
            steps: vec!["Check the container's args in the Deployment.".into()],
            confidence: "medium".into(),
        }
    }

    fn narrow() -> Explanation {
        explanation("The container exits right after it starts.")
    }

    fn item() -> Queued {
        queued(&incident(), narrow())
    }

    fn item_for(fingerprint: &str, summary: &str) -> Queued {
        let mut incident = incident();
        incident.fingerprint = fingerprint.into();
        queued(&incident, explanation(summary))
    }

    fn shared(metrics: &Metrics, ttl: Duration) -> Shared<'_> {
        Shared {
            budget: RefCell::new(Budget::new(30, 200)),
            dedup: RefCell::new(Dedup::new(ttl, 1024)),
            writer: None,
            history: None,
            metrics,
            notifications: RefCell::new(VecDeque::new()),
        }
    }

    const TTL: Duration = Duration::from_secs(6 * 3600);

    /// The value of one series in `/metrics`, 0 when it was never incremented.
    fn counter(metrics: &Metrics, series: &str) -> u64 {
        let text = metrics.render();
        let name = series.split('{').next().unwrap_or(series);
        assert!(text.contains(&format!("# TYPE {name} counter\n")), "{text}");
        text.lines()
            .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
            .map_or(0, |value| value.parse().expect("a count"))
    }

    fn outcomes(metrics: &Metrics) -> (u64, u64, u64, u64) {
        (
            counter(metrics, "kausa_notifications_total{outcome=\"sent\"}"),
            counter(metrics, "kausa_notifications_total{outcome=\"failed\"}"),
            counter(metrics, "kausa_notifications_total{outcome=\"dropped\"}"),
            counter(metrics, "kausa_notifications_retried_total"),
        )
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    /// One request the fake webhook received: the request line, the head in lowercase and the
    /// body.
    struct Sent {
        line: String,
        head: String,
        body: String,
    }

    impl Sent {
        fn json(&self) -> Value {
            serde_json::from_str(&self.body).expect("a JSON body")
        }
    }

    /// What the fake webhook does with one connection.
    enum Answer {
        /// A status line such as `200 OK`, with an empty body.
        Status(&'static str),
        /// A `302` to `location`.
        Redirect(String),
        /// Reads the request and answers nothing for this long.
        Hang(Duration),
    }

    /// A fake webhook on localhost: one connection per answer, in order. Each request is sent
    /// back on the channel. Returns `http://127.0.0.1:<port>`.
    fn webhook(answers: Vec<Answer>) -> (String, mpsc::Receiver<Sent>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = vec![0; 65536];
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    request.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length || n == 0 {
                            let (line, head) = text[..end].split_once("\r\n").unwrap_or_default();
                            let _ = sent.send(Sent {
                                line: line.to_owned(),
                                head: head.to_ascii_lowercase(),
                                body: text[end + 4..].to_owned(),
                            });
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let response = match answer {
                    Answer::Status(status) => format!(
                        "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    ),
                    Answer::Redirect(location) => format!(
                        "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\n\
                         connection: close\r\n\r\n"
                    ),
                    Answer::Hang(time) => {
                        std::thread::sleep(time);
                        continue;
                    }
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (base, received)
    }

    /// A local port with nothing listening on it.
    fn closed_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port()
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a URL")
    }

    fn notifier(base: &str, format: Format, token: Option<String>) -> Notifier {
        Notifier::new(url(&format!("{base}/hook/x")), format, token, true).expect("a notifier")
    }

    fn telegram(chat: &str) -> Format {
        Format::Telegram(parse_chat_id(chat).expect("a chat id"))
    }

    // --- Constants and formats -------------------------------------------------------------------

    #[test]
    fn the_limits_of_the_contract() {
        assert_eq!(QUEUE_LIMIT, 16);
        assert_eq!(MIN_INTERVAL, Duration::from_secs(1));
        assert_eq!(TELEGRAM_MIN_INTERVAL, Duration::from_secs(3));
        assert_eq!(crate::explain::RETRY_DELAY, Duration::from_secs(30));
    }

    #[test]
    fn formats_have_their_names() {
        assert_eq!(Format::Json.name(), "json");
        assert_eq!(Format::Slack.name(), "slack");
        assert_eq!(telegram("-1001234567890").name(), "telegram");
    }

    #[test]
    fn telegram_waits_three_seconds_between_sends_and_the_others_one() {
        assert_eq!(Format::Json.min_interval(), Duration::from_secs(1));
        assert_eq!(Format::Slack.min_interval(), Duration::from_secs(1));
        assert_eq!(
            telegram("-1001234567890").min_interval(),
            Duration::from_secs(3)
        );
        assert_eq!(
            telegram("@kausa_alerts").min_interval(),
            Duration::from_secs(3)
        );
    }

    // --- URL checks ------------------------------------------------------------------------------

    const NOT_A_URL: &str = "is not a valid URL";
    const NOT_HTTPS: &str = "must be https:// (or http:// with --notify-allow-http)";
    const USER_INFO: &str = "must not carry a user name or password";
    const TELEGRAM_PATH: &str = "must end in /bot<token>/sendMessage with --notify-format telegram";

    #[test]
    fn a_url_must_parse() {
        for text in [
            "",
            "hooks.example.com/x",
            "https://",
            "http://[::1",
            "not a url",
            "://x",
        ] {
            for format in [Format::Json, Format::Slack, telegram("-1")] {
                assert_eq!(
                    check_url(text, &format, true).err(),
                    Some(NOT_A_URL),
                    "{text:?} as {}",
                    format.name()
                );
            }
        }
    }

    #[test]
    fn a_url_must_be_https_unless_http_is_allowed() {
        let good = "https://hooks.example.com/services/T0/B0/fake";
        assert_eq!(
            check_url(good, &Format::Json, false)
                .expect("an https URL")
                .as_str(),
            good
        );
        assert!(check_url("HTTPS://hooks.example.com/x", &Format::Slack, false).is_ok());
        let plain = "http://receiver.kausa.svc:8080/hook";
        assert_eq!(
            check_url(plain, &Format::Json, false).err(),
            Some(NOT_HTTPS)
        );
        assert!(check_url(plain, &Format::Json, true).is_ok());
        for other in [
            "ftp://hooks.example.com/x",
            "file:///etc/passwd",
            "ws://hooks.example.com/x",
            "mailto:ops@example.com",
            "data:text/plain,x",
        ] {
            for allow in [false, true] {
                assert_eq!(
                    check_url(other, &Format::Json, allow).err(),
                    Some(NOT_HTTPS),
                    "{other} with allow_http {allow}"
                );
            }
        }
    }

    #[test]
    fn a_url_must_not_carry_user_information() {
        for text in [
            "https://user:secret@hooks.example.com/x",
            "https://user@hooks.example.com/x",
            "https://:secret@hooks.example.com/x",
            "http://user:secret@receiver.kausa.svc/x",
        ] {
            assert_eq!(
                check_url(text, &Format::Json, true).err(),
                Some(USER_INFO),
                "{text}"
            );
        }
        // The scheme is checked first.
        assert_eq!(
            check_url("ftp://user:secret@hooks.example.com/x", &Format::Json, true).err(),
            Some(NOT_HTTPS)
        );
    }

    #[test]
    fn a_telegram_url_must_end_in_bot_token_send_message() {
        let token = fake_telegram_token();
        let format = telegram("-1001234567890");
        for good in [
            format!("https://api.telegram.org/bot{token}/sendMessage"),
            // A self-hosted Bot API server, and a query, which is not checked.
            format!("https://tg.example.com/bot{token}/sendMessage"),
            format!("https://tg.example.com:8443/bot{token}/sendMessage?x=1"),
            "https://api.telegram.org/bot1/sendMessage".to_owned(),
        ] {
            let checked = check_url(&good, &format, false);
            assert!(checked.is_ok(), "{good}: {checked:?}");
        }
        assert!(
            check_url(
                &format!("http://127.0.0.1:8081/bot{token}/sendMessage"),
                &format,
                true
            )
            .is_ok()
        );
        for bad in [
            "https://api.telegram.org/bot/sendMessage".to_owned(),
            format!("https://api.telegram.org/bot{token}/getMe"),
            format!("https://api.telegram.org/bot{token}/sendMessage/"),
            format!("https://api.telegram.org/bot{token}/sendmessage"),
            format!("https://api.telegram.org/prefix/bot{token}/sendMessage"),
            format!("https://api.telegram.org/bot{token}/x/sendMessage"),
            format!("https://api.telegram.org/{token}/sendMessage"),
            "https://api.telegram.org/".to_owned(),
            "https://api.telegram.org".to_owned(),
        ] {
            let error = check_url(&bad, &format, false).err();
            assert_eq!(error, Some(TELEGRAM_PATH), "{bad}");
        }
        // The other formats take any path.
        let any = format!("https://hooks.example.com/bot{token}/getMe");
        assert!(check_url(&any, &Format::Json, false).is_ok());
        assert!(check_url(&any, &Format::Slack, false).is_ok());
        // The checks before the path come first.
        let plain = format!("http://api.telegram.org/bot{token}/getMe");
        assert_eq!(check_url(&plain, &format, false).err(), Some(NOT_HTTPS));
        let user = format!("https://u:p@api.telegram.org/bot{token}/getMe");
        assert_eq!(check_url(&user, &format, false).err(), Some(USER_INFO));
    }

    #[test]
    fn the_host_is_only_the_host() {
        let token = fake_telegram_token();
        let format = telegram("@kausa_alerts");
        let telegram_url = url(&format!("https://api.telegram.org/bot{token}/sendMessage"));
        let n = Notifier::new(telegram_url, format, None, false).expect("a notifier");
        assert_eq!(n.host(), "api.telegram.org");
        let hook = url("https://hooks.example.com:8443/services/T0/B0/fake?q=1");
        let n = Notifier::new(hook, Format::Slack, None, false).expect("a notifier");
        assert_eq!(n.host(), "hooks.example.com");
    }

    // --- One attempt -----------------------------------------------------------------------------

    fn notice() -> Notice {
        item().notice
    }

    #[test]
    fn a_json_post_carries_the_body_the_type_and_the_user_agent() {
        let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
        let status = block_on(notifier(&base, Format::Json, None).send(&notice()));
        assert_eq!(status.ok(), Some(200));
        let sent = requests.recv().expect("a request");
        assert!(sent.line.starts_with("POST /hook/x "), "{}", sent.line);
        assert!(
            sent.head
                .lines()
                .any(|l| l.trim() == "content-type: application/json"),
            "{}",
            sent.head
        );
        assert!(
            sent.head.contains(&format!("user-agent: kausa/{VERSION}")),
            "{}",
            sent.head
        );
        assert!(!sent.head.contains("authorization:"), "{}", sent.head);
        assert_eq!(sent.json(), notification::json_body(&notice(), VERSION));
    }

    #[test]
    fn the_bearer_token_goes_only_with_json() {
        let token = fake_bearer();
        let (base, requests) = webhook(vec![Answer::Status("204 No Content")]);
        let status = block_on(notifier(&base, Format::Json, Some(token.clone())).send(&notice()));
        assert_eq!(status.ok(), Some(204));
        let sent = requests.recv().expect("a request");
        assert!(
            sent.head.contains(&format!(
                "authorization: bearer {}",
                token.to_ascii_lowercase()
            )),
            "{}",
            sent.head
        );
        // A token given with another format is never sent.
        for format in [Format::Slack, telegram("-1001234567890")] {
            let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
            let name = format.name();
            let path = if name == "telegram" {
                format!("{base}/bot{}/sendMessage", fake_telegram_token())
            } else {
                format!("{base}/hook/x")
            };
            let Ok(n) = Notifier::new(url(&path), format, Some(token.clone()), true) else {
                continue;
            };
            assert!(block_on(n.send(&notice())).is_ok(), "{name}");
            let sent = requests.recv().expect("a request");
            assert!(
                !sent.head.contains("authorization:"),
                "{name}: {}",
                sent.head
            );
        }
    }

    #[test]
    fn slack_and_telegram_post_their_bodies() {
        let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
        assert!(block_on(notifier(&base, Format::Slack, None).send(&notice())).is_ok());
        let sent = requests.recv().expect("a request");
        assert_eq!(sent.json(), notification::slack_body(&notice()));
        assert!(sent.head.contains("content-type: application/json"));
        assert!(sent.head.contains(&format!("user-agent: kausa/{VERSION}")));

        let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
        let path = format!("{base}/bot{}/sendMessage", fake_telegram_token());
        let n =
            Notifier::new(url(&path), telegram("-1001234567890"), None, true).expect("a notifier");
        assert!(block_on(n.send(&notice())).is_ok());
        let sent = requests.recv().expect("a request");
        assert!(sent.line.starts_with("POST /bot"), "{}", sent.line);
        let chat = ChatId::Id(-1001234567890);
        assert_eq!(sent.json(), notification::telegram_body(&notice(), &chat));
        assert!(sent.json().get("parse_mode").is_none());
    }

    #[test]
    fn statuses_are_sent_failed_or_transient() {
        for (status, code) in [
            ("200 OK", 200),
            ("202 Accepted", 202),
            ("204 No Content", 204),
        ] {
            let (base, _requests) = webhook(vec![Answer::Status(status)]);
            let result = block_on(notifier(&base, Format::Json, None).send(&notice()));
            assert_eq!(result.ok(), Some(code), "{status}");
        }
        for (status, code, transient) in [
            ("400 Bad Request", 400, false),
            ("401 Unauthorized", 401, false),
            ("403 Forbidden", 403, false),
            ("404 Not Found", 404, false),
            ("429 Too Many Requests", 429, true),
            ("500 Internal Server Error", 500, true),
            ("502 Bad Gateway", 502, true),
            ("503 Service Unavailable", 503, true),
        ] {
            let (base, _requests) = webhook(vec![Answer::Status(status)]);
            let failure =
                block_on(notifier(&base, Format::Json, None).send(&notice())).expect_err(status);
            assert_eq!(
                failure.reason,
                format!("the webhook answered {code}"),
                "{status}"
            );
            assert_eq!(failure.transient, transient, "{status}");
        }
    }

    #[test]
    fn a_redirect_is_not_followed() {
        // Where the redirect points: a listener that reports any connection.
        let target = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let location = format!("http://{}/stolen", target.local_addr().expect("an address"));
        target.set_nonblocking(true).expect("non-blocking");
        let (base, requests) = webhook(vec![Answer::Redirect(location)]);
        let failure = block_on(notifier(&base, Format::Json, None).send(&notice()))
            .expect_err("a redirect fails");
        assert_eq!(
            failure.reason,
            "a redirect is not followed: put the final URL in the file"
        );
        assert!(!failure.transient);
        assert!(requests.recv().is_ok());
        std::thread::sleep(Duration::from_millis(200));
        assert!(target.accept().is_err(), "the redirect was followed");
    }

    #[test]
    fn a_timeout_is_a_transient_failure_without_the_url() {
        let token = fake_telegram_token();
        let (base, _requests) = webhook(vec![Answer::Hang(Duration::from_secs(15))]);
        let path = format!("{base}/bot{token}/sendMessage");
        let n =
            Notifier::new(url(&path), telegram("-1001234567890"), None, true).expect("a notifier");
        let started = Instant::now();
        let failure = block_on(n.send(&notice())).expect_err("a timeout");
        assert!(
            started.elapsed() <= Duration::from_secs(12),
            "{:?}",
            started.elapsed()
        );
        assert!(failure.transient, "{failure:?}");
        assert!(
            failure.reason.starts_with("the request failed: "),
            "{failure:?}"
        );
        for text in [failure.reason.clone(), format!("{failure:?}")] {
            assert!(!text.contains(&token), "{text}");
            assert!(!text.contains("/bot"), "{text}");
            assert!(!text.contains(&path), "{text}");
        }
    }

    #[test]
    fn a_refused_connection_is_transient_and_never_shows_the_token() {
        let token = fake_telegram_token();
        let path = format!("http://127.0.0.1:{}/bot{token}/sendMessage", closed_port());
        let n =
            Notifier::new(url(&path), telegram("-1001234567890"), None, true).expect("a notifier");
        let failure = block_on(n.send(&notice())).expect_err("a refused connection");
        assert!(failure.transient, "{failure:?}");
        assert!(
            failure.reason.starts_with("the request failed: "),
            "{failure:?}"
        );
        for text in [failure.reason.clone(), format!("{failure:?}")] {
            assert!(!text.contains(&token), "{text}");
            assert!(!text.contains(&path), "{text}");
        }
        // Nor does the bearer token of a json webhook.
        let bearer = fake_bearer();
        let hook = format!("http://127.0.0.1:{}/hook/x", closed_port());
        let n = Notifier::new(url(&hook), Format::Json, Some(bearer.clone()), true)
            .expect("a notifier");
        let failure = block_on(n.send(&notice())).expect_err("a refused connection");
        assert!(!format!("{failure:?}").contains(&bearer), "{failure:?}");
        assert!(!format!("{failure:?}").contains(&hook), "{failure:?}");
    }

    #[test]
    fn check_url_errors_never_carry_the_url() {
        let token = fake_telegram_token();
        for text in [
            format!("gopher://example.com/bot{token}/sendMessage"),
            format!("https://u:{token}@api.telegram.org/bot{token}/sendMessage"),
            format!("https://api.telegram.org/bot{token}/getMe"),
            format!("not a url {token}"),
        ] {
            let error = check_url(&text, &telegram("-1"), false).expect_err("refused");
            assert!(!error.contains(&token), "{error}");
        }
    }

    // --- Queued notifications --------------------------------------------------------------------

    #[test]
    fn a_queued_notification_names_the_rule_the_object_and_its_first_sighting() {
        let item = item();
        assert_eq!(item.fingerprint, FINGERPRINT);
        assert_eq!(item.explanation, narrow());
        assert_eq!(
            item.notice,
            Notice {
                detection_id: "D001".into(),
                detection_name: "crash loop".into(),
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                namespace: "shop".into(),
                name: "web".into(),
                fingerprint: FINGERPRINT.into(),
                // The same string as the incident's log line.
                first_seen: incident().first_seen.to_string(),
                summary: "The container exits right after it starts.".into(),
                cause: "It is started with a flag it does not know.".into(),
                steps: vec!["Check the container's args in the Deployment.".into()],
                confidence: "medium".into(),
            }
        );
        assert_eq!(item.notice.first_seen, "2026-10-28T11:59:00Z");
        // Which makes the reference example.
        let body = notification::json_body(&item.notice, "0.2.0");
        assert_eq!(body["object"]["name"], "web");
        assert_eq!(body["first_seen"], "2026-10-28T11:59:00Z");
    }

    /// A Gathered of Deployment `web` in `shop` whose log tail, event message, Pod message,
    /// waiting message and sample message carry canaries.
    fn gathered_with_canaries() -> Gathered {
        let incident = incident();
        Gathered {
            pods: vec![PodRecord {
                namespace: "shop".into(),
                name: "web-7d9c8b6f4d-2xkqp".into(),
                uid: "uid-pod".into(),
                controller: None,
                phase: Some("Running".into()),
                unschedulable: None,
                unschedulable_reason: None,
                reason: None,
                message: Some("POD-CANARY the pod message".into()),
                failed_at: None,
                node_name: Some("worker-7".into()),
                service_account: Some("web-runner".into()),
                image_pull_secrets: 0,
                scheduling: None,
                containers: vec![ContainerRecord {
                    name: "server".into(),
                    image: Some("registry.example.com/team/web:1.4.2".into()),
                    waiting_reason: Some("CrashLoopBackOff".into()),
                    waiting_message: Some("WAITING-CANARY back-off 40s".into()),
                    last_termination_exit_code: Some(1),
                    restart_count: 4,
                    ..Default::default()
                }],
            }],
            events: vec![EventRecord {
                uid: "e1".into(),
                reason: "BackOff".into(),
                note: Some("EVENT-CANARY Back-off restarting failed container".into()),
                regarding: Regarding {
                    api_version: "v1".into(),
                    kind: "Pod".into(),
                    namespace: "shop".into(),
                    name: "web-7d9c8b6f4d-2xkqp".into(),
                    uid: None,
                },
                reporting_controller: None,
                first_seen: None,
                last_seen: None,
                count: 4,
            }],
            pods_total: 1,
            log_tail: Some("LOG-CANARY panic: unknown flag --colour\n".into()),
            incident,
            rollout: None,
        }
    }

    const CANARIES: [&str; 5] = [
        "LOG-CANARY",
        "EVENT-CANARY",
        "POD-CANARY",
        "WAITING-CANARY",
        "SAMPLE-CANARY",
    ];

    #[test]
    fn no_body_carries_the_context_or_a_name_but_the_namespace_and_the_object() {
        let gathered = gathered_with_canaries();
        let (request, redactor) = context::build(&gathered);
        // The model's answer (a fake one, which never echoes its input) names everything.
        let answer = serde_json::json!({
            "summary": "deployment-1 in namespace-1 crashes: pod-1 on node-1 exits.",
            "cause": "container-1 of registry-1/image-1:1.4.2 runs as serviceaccount-1 \
                      and reaches <redacted:ip>.",
            "steps": ["Check Deployment-1's args in namespace-1."],
            "confidence": "medium",
        })
        .to_string();
        let explanation = prompt::validate(&answer).expect("a valid answer");
        let incident = &gathered.incident;
        let names = [
            incident.key.namespace.as_str(),
            incident.owner.name.as_str(),
        ];
        let narrow = explanation.map_clean(|t| redactor.restore_only(t, &names));
        let item = queued(incident, narrow);
        assert_eq!(
            item.explanation.summary,
            "web in shop crashes: pod-1 on node-1 exits."
        );
        assert_eq!(
            item.explanation.cause,
            "container-1 of registry-1/image-1:1.4.2 runs as serviceaccount-1 and reaches \
             <redacted:ip>."
        );
        assert_eq!(item.explanation.steps, ["Check web's args in shop."]);
        let bodies = [
            notification::json_body(&item.notice, VERSION).to_string(),
            notification::slack_body(&item.notice).to_string(),
            notification::telegram_body(&item.notice, &ChatId::Id(-1)).to_string(),
        ];
        for body in &bodies {
            for leaked in CANARIES.iter().copied().chain([
                "web-7d9c8b6f4d",
                "worker-7",
                "web-runner",
                "server",
                "registry.example.com",
                "team/web",
                "untrusted_cluster_data",
                "log_tail",
                "warning_events",
                "namespace-1",
                "deployment-1",
                "Deployment-1",
                "CrashLoopBackOff",
                "BackOff",
                "an-invented-uid",
                "uid-pod",
            ]) {
                assert!(!body.contains(leaked), "{leaked} in {body}");
            }
            assert!(body.contains("shop") && body.contains("web"), "{body}");
            assert!(body.contains("pod-1") && body.contains("node-1"), "{body}");
        }
        // The request to the model is what carried the context, redacted.
        let request = request.to_string();
        assert!(request.contains("namespace-1"), "{request}");
    }

    // --- The queue and the dequeue rule ----------------------------------------------------------

    #[test]
    fn a_full_queue_drops_its_oldest_notification_and_counts_it() {
        let metrics = Metrics::default();
        let shared = shared(&metrics, TTL);
        for i in 0..QUEUE_LIMIT {
            enqueue(&shared, item_for(&format!("fp-{i}"), "s")).expect("the log");
        }
        assert_eq!(shared.notifications.borrow().len(), QUEUE_LIMIT);
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0));
        enqueue(&shared, item_for(&format!("fp-{QUEUE_LIMIT}"), "s")).expect("the log");
        let queue = shared.notifications.borrow();
        assert_eq!(queue.len(), QUEUE_LIMIT);
        assert_eq!(queue.front().map(|q| q.fingerprint.as_str()), Some("fp-1"));
        assert_eq!(queue.back().map(|q| q.fingerprint.as_str()), Some("fp-16"));
        assert_eq!(outcomes(&metrics), (0, 0, 1, 0));
    }

    #[test]
    fn the_dequeue_rule_skips_only_what_was_sent() {
        let mut dedup = Dedup::new(TTL, 1024);
        let mut queue: VecDeque<Queued> = VecDeque::new();
        assert!(take_next(&mut queue, &dedup).is_none());

        // Not notified yet: sent.
        dedup.put(
            FINGERPRINT,
            explanation("full"),
            Some(narrow()),
            Instant::now(),
        );
        queue.push_back(item());
        let next = take_next(&mut queue, &dedup).expect("an item");
        assert_eq!(next.fingerprint, FINGERPRINT);
        assert!(queue.is_empty());

        // Notified with the same text: two queued copies give nothing more, and the queue is
        // emptied of them.
        assert!(dedup.mark_notified(FINGERPRINT, &narrow()));
        queue.push_back(item());
        queue.push_back(item());
        assert!(take_next(&mut queue, &dedup).is_none());
        assert!(queue.is_empty());

        // Duplicates in front of another notification are dropped on the way to it.
        queue.push_back(item());
        queue.push_back(item_for("other", "s"));
        queue.push_back(item());
        let next = take_next(&mut queue, &dedup).expect("an item");
        assert_eq!(next.fingerprint, "other");
        assert_eq!(queue.len(), 1);
        assert!(take_next(&mut queue, &dedup).is_none());

        // An entry that stores another text, even notified, does not stop it.
        queue.push_back(queued(&incident(), explanation("an older text")));
        let next = take_next(&mut queue, &dedup).expect("an item");
        assert_eq!(next.explanation, explanation("an older text"));

        // An evicted or expired entry does not stop it either.
        let empty = Dedup::new(TTL, 1024);
        queue.push_back(item());
        assert!(take_next(&mut queue, &empty).is_some());
    }

    // --- Delivery ---------------------------------------------------------------------------------

    /// Delivers `item` to a fake webhook that gives `answers`, with the Dedup entry `entry`
    /// (`Some(notice)` stored for FINGERPRINT before the delivery), and returns the metrics, the
    /// requests and the entry's `notified` afterwards (`None` without an entry).
    fn deliver_with(
        answers: Vec<Answer>,
        entry: Option<Explanation>,
        item: Queued,
    ) -> (Metrics, Vec<Sent>, Option<bool>, Duration) {
        let count = answers.len();
        let (base, requests) = webhook(answers);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let (notified, elapsed) = {
            let shared = shared(&metrics, TTL);
            if let Some(notice) = entry {
                shared.dedup.borrow_mut().put(
                    FINGERPRINT,
                    explanation("full"),
                    Some(notice),
                    Instant::now(),
                );
            }
            let started = Instant::now();
            block_on(deliver(&n, item, &shared)).expect("the log");
            let elapsed = started.elapsed();
            let notified = shared.dedup.borrow().peek(FINGERPRINT).map(|c| c.notified);
            (notified, elapsed)
        };
        let sent: Vec<Sent> = (0..count)
            .filter_map(|_| requests.recv_timeout(Duration::from_millis(500)).ok())
            .collect();
        (metrics, sent, notified, elapsed)
    }

    #[test]
    fn a_2xx_is_sent_marks_its_entry_and_keeps_the_interval() {
        let (metrics, sent, notified, elapsed) =
            deliver_with(vec![Answer::Status("200 OK")], Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert_eq!(sent.len(), 1);
        assert_eq!(notified, Some(true));
        assert!(elapsed >= MIN_INTERVAL, "{elapsed:?}");
    }

    #[test]
    fn telegram_keeps_three_seconds_between_sends_and_slack_one() {
        let deliver_in = |format: &str| {
            let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
            let n = match format {
                "telegram" => {
                    let path = format!("{base}/bot{}/sendMessage", fake_telegram_token());
                    Notifier::new(url(&path), telegram("-1001234567890"), None, true)
                        .expect("a notifier")
                }
                _ => notifier(&base, Format::Slack, None),
            };
            let metrics = Metrics::default();
            let shared = shared(&metrics, TTL);
            let started = Instant::now();
            block_on(deliver(&n, item(), &shared)).expect("the log");
            let elapsed = started.elapsed();
            assert!(requests.recv_timeout(Duration::from_millis(500)).is_ok());
            assert_eq!(outcomes(&metrics), (1, 0, 0, 0), "{format}");
            elapsed
        };
        let elapsed = deliver_in("telegram");
        assert!(elapsed >= TELEGRAM_MIN_INTERVAL, "{elapsed:?}");
        let elapsed = deliver_in("slack");
        assert!(elapsed >= MIN_INTERVAL, "{elapsed:?}");
        assert!(elapsed < TELEGRAM_MIN_INTERVAL, "{elapsed:?}");
    }

    #[test]
    fn a_503_then_a_200_is_one_retry() {
        let answers = vec![
            Answer::Status("503 Service Unavailable"),
            Answer::Status("200 OK"),
        ];
        let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 1));
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].body, sent[1].body);
        assert_eq!(notified, Some(true));
    }

    #[test]
    fn a_429_then_a_200_is_one_retry() {
        let answers = vec![
            Answer::Status("429 Too Many Requests"),
            Answer::Status("204 No Content"),
        ];
        let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 1));
        assert_eq!(sent.len(), 2);
        assert_eq!(notified, Some(true));
    }

    #[test]
    fn two_transient_errors_fail_after_one_retry() {
        let answers = vec![
            Answer::Status("503 Service Unavailable"),
            Answer::Status("502 Bad Gateway"),
            Answer::Status("200 OK"),
        ];
        let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (0, 1, 0, 1));
        assert_eq!(sent.len(), 2, "a third attempt was made");
        assert_eq!(notified, Some(false));
    }

    #[test]
    fn a_400_or_a_redirect_fails_at_once() {
        for answer in [
            Answer::Status("400 Bad Request"),
            Answer::Status("404 Not Found"),
            Answer::Redirect("http://127.0.0.1:9/elsewhere".into()),
        ] {
            let answers = vec![answer, Answer::Status("200 OK")];
            let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
            assert_eq!(outcomes(&metrics), (0, 1, 0, 0));
            assert_eq!(sent.len(), 1, "it was tried again");
            assert_eq!(notified, Some(false));
        }
    }

    #[test]
    fn a_refused_connection_fails_after_one_retry() {
        let n = Notifier::new(
            url(&format!("http://127.0.0.1:{}/hook/x", closed_port())),
            Format::Json,
            None,
            true,
        )
        .expect("a notifier");
        let metrics = Metrics::default();
        let shared = shared(&metrics, TTL);
        block_on(deliver(&n, item(), &shared)).expect("the log");
        assert_eq!(outcomes(&metrics), (0, 1, 0, 1));
    }

    #[test]
    fn an_evicted_entry_is_still_sent_and_nothing_is_marked() {
        let (metrics, sent, notified, _) =
            deliver_with(vec![Answer::Status("200 OK")], None, item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert_eq!(sent.len(), 1);
        assert_eq!(notified, None);
    }

    #[test]
    fn an_older_send_does_not_mark_the_entry_that_replaced_its_own() {
        // The entry now stores a newer text than the queued notification's.
        let newer = explanation("A newer explanation.");
        let (metrics, sent, notified, _) =
            deliver_with(vec![Answer::Status("200 OK")], Some(newer), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert_eq!(sent.len(), 1);
        assert_eq!(notified, Some(false));
    }

    #[test]
    fn two_queued_notifications_for_one_entry_give_one_message() {
        let (base, requests) = webhook(vec![Answer::Status("200 OK"), Answer::Status("200 OK")]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let shared = shared(&metrics, TTL);
        shared.dedup.borrow_mut().put(
            FINGERPRINT,
            explanation("full"),
            Some(narrow()),
            Instant::now(),
        );
        enqueue(&shared, item()).expect("the log");
        enqueue(&shared, item()).expect("the log");
        block_on(async {
            loop {
                let next = {
                    let dedup = shared.dedup.borrow();
                    take_next(&mut shared.notifications.borrow_mut(), &dedup)
                };
                let Some(next) = next else { break };
                deliver(&n, next, &shared).await.expect("the log");
            }
        });
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert!(requests.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(requests.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn with_a_ttl_of_zero_every_sequential_occurrence_is_sent() {
        let (base, requests) = webhook(vec![
            Answer::Status("200 OK"),
            Answer::Status("200 OK"),
            Answer::Status("200 OK"),
        ]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let shared = shared(&metrics, Duration::ZERO);
        block_on(async {
            for _ in 0..3 {
                // What explain::run does with --dedup-hours 0: no fresh entry, a model call, a
                // new entry and a queued notification.
                let now = Instant::now();
                assert!(shared.dedup.borrow_mut().get(FINGERPRINT, now).is_none());
                shared.dedup.borrow_mut().put(
                    FINGERPRINT,
                    explanation("full"),
                    Some(narrow()),
                    now,
                );
                enqueue(&shared, item()).expect("the log");
                let next = {
                    let dedup = shared.dedup.borrow();
                    take_next(&mut shared.notifications.borrow_mut(), &dedup)
                };
                deliver(&n, next.expect("not skipped"), &shared)
                    .await
                    .expect("the log");
            }
        });
        assert_eq!(outcomes(&metrics), (3, 0, 0, 0));
        assert_eq!(requests.try_iter().count(), 3);
    }
}
