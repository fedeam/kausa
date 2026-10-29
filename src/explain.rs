//! From an incident to an explanation: read the log tail, build the redacted context, then either
//! log the request (`--dry-run`) or send it to the model and validate the answer.
//!
//! One explanation runs at a time, and the queue in front of it is bounded, so neither a slow
//! endpoint nor a burst of incidents can stop the watches or grow kausa's memory.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use futures::AsyncReadExt;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::jiff::Timestamp;
use kube::api::LogParams;
use kube::{Api, Client};
use serde_json::{Value, json};

use crate::budget::{Budget, Dedup};
use crate::context::{self, Gathered};
use crate::detect::Detection;
use crate::events::{Outcome, Writer};
use crate::history::History;
use crate::log;
use crate::metrics::Metrics;
use crate::model::ModelClient;
use crate::notify;
use crate::prompt::{self, OutputMode, PROMPT_VERSION};

/// How many incidents may wait for an explanation; the oldest is dropped when one more arrives.
pub const QUEUE_LIMIT: usize = 16;
/// The log lines asked of the API server, the most it may send, and the end of it that kausa keeps
/// (before redaction and the context's own 4 KiB cap). The API server reads forward from the start
/// of the last lines, so its limit would cut their end: kausa keeps the end itself.
const LOG_TAIL_LINES: i64 = 50;
const LOG_LIMIT_BYTES: i64 = 1024 * 1024;
const LOG_KEEP_BYTES: usize = 32 * 1024;
const LOG_TIMEOUT: Duration = Duration::from_secs(10);

/// How incidents are explained.
pub struct Settings {
    /// The endpoint, or `None` in dry-run.
    pub model: Option<ModelClient>,
    pub model_name: String,
    pub logs: bool,
    pub mode: OutputMode,
    pub max_tokens: u32,
    /// The most input tokens a request may carry, estimated as a quarter of its bytes.
    pub max_input_tokens: usize,
    /// Whether explanations are written as Events (not in dry-run).
    pub events: bool,
    /// With `--history`, the `--history-days`: explanations are also kept in Explanation objects.
    pub history_days: Option<u32>,
    /// Model calls allowed per hour and per day.
    pub calls_per_hour: usize,
    pub calls_per_day: usize,
    /// How long an explanation is reused for the same incident fingerprint.
    pub dedup_ttl: Duration,
    /// The reporting instance of kausa's Events: its Pod's name, or `kausa`.
    pub instance: String,
    /// With `--notify-url-file`, the webhook each new explanation is posted to
    /// (docs/adr/0015-notifications.md).
    pub notify: Option<notify::Notifier>,
}

/// What the explanations share across incidents: the budget, the deduplication cache, the Event
/// writer, the history writer, the metrics and the queue of notifications. Borrowed only between
/// awaits.
pub struct Shared<'a> {
    pub budget: RefCell<Budget>,
    pub dedup: RefCell<Dedup>,
    pub writer: Option<Writer>,
    /// The history writer, with `--history` and an endpoint.
    pub history: Option<History>,
    pub metrics: &'a Metrics,
    /// The notifications waiting for the webhook, at most `notify::QUEUE_LIMIT`.
    pub notifications: RefCell<VecDeque<notify::Queued>>,
}

/// Which container's log to read for an incident.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSource {
    pub namespace: String,
    pub pod: String,
    pub container: String,
    /// The previous instance (it restarted and runs or waits again), or the terminated one.
    pub previous: bool,
}

/// One incident waiting for its explanation.
#[derive(Clone)]
pub struct Work {
    pub gathered: Gathered,
    pub log: Option<LogSource>,
    /// How many calls for it failed with a transient error (it is tried at most twice).
    pub attempt: u8,
}

/// How long a transient failure waits before its one retry.
pub const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether a detection reads the failed container's log (docs/reference/rules.md).
pub fn reads_logs(detection: Detection) -> bool {
    matches!(
        detection,
        Detection::CrashLoop | Detection::OutOfMemory | Detection::JobFailed
    )
}

/// Explains one incident and logs the outcome: from the deduplication cache when the same failure
/// was explained recently; otherwise reads the log tail, builds and fits the request, and logs it
/// (dry-run) or sends it within the budget and validates the answer. Explanations and skipped
/// explanations are written as Events when that is on, and explanations, fresh or cached, into
/// the history with `--history`. With notifications on, an explanation is queued for the webhook
/// once its Event and history are written: a new one always, a cached one while its notification
/// was not sent; a skip never is. A first call that fails with a transient error (a timeout, a
/// failed connection, `429` or a `5xx` status) returns the job, to be tried once more after
/// `RETRY_DELAY`; a call that never reached the endpoint is given back to the budget.
pub async fn run(
    job: Work,
    client: Client,
    settings: &Settings,
    shared: &Shared<'_>,
) -> io::Result<Option<Work>> {
    let retry = (job.attempt == 0).then(|| Work {
        attempt: 1,
        ..job.clone()
    });
    let mut gathered = job.gathered;
    let fingerprint = gathered.incident.fingerprint.clone();
    let about = json!({
        "detection": gathered.incident.key.detection.id(),
        "namespace": gathered.incident.key.namespace,
        "owner": {"kind": gathered.incident.owner.kind, "name": gathered.incident.owner.name},
        "fingerprint": fingerprint,
    });

    if settings.model.is_some() {
        let cached = shared
            .dedup
            .borrow_mut()
            .get(&fingerprint, std::time::Instant::now());
        if let Some(cached) = cached {
            shared.metrics.inc("kausa_explanations_cached_total", &[]);
            log::info(
                "explanation",
                merge(
                    about,
                    json!({"explanation": cached.explanation.to_json(), "cached": true}),
                ),
            )?;
            emit(
                settings,
                shared,
                &gathered,
                Outcome::Explained(&cached.explanation),
            )
            .await?;
            // The stored notification, again, until the webhook has acknowledged it.
            if let Some(notice) = cached.notice.filter(|_| settings.notify.is_some())
                && !cached.notified
            {
                notify::enqueue(shared, notify::queued(&gathered.incident, notice))?;
            }
            return Ok(None);
        }
    }

    if settings.logs
        && let Some(source) = &job.log
    {
        match read_log(&client, source).await {
            Ok(text) => gathered.log_tail = Some(text),
            Err(error) => log::warn(
                "log read failed",
                json!({
                    "namespace": source.namespace,
                    "pod": source.pod,
                    "container": source.container,
                    "error": error,
                }),
            )?,
        }
    }
    let Some((body, redactor)) = fit(&mut gathered, settings) else {
        return skip(
            settings,
            shared,
            &gathered,
            about,
            "context_too_large",
            "the context does not fit --max-input-tokens even without logs",
        )
        .await
        .map(|()| None);
    };
    let redactions = counts(redactor.counts());
    for (rule, n) in redactor.counts() {
        shared
            .metrics
            .add("kausa_redactions_total", &[("rule", rule)], *n as u64);
    }
    let Some(model) = &settings.model else {
        // Dry-run: the exact body that would be sent, which holds only redacted data.
        shared.metrics.inc("kausa_requests_logged_total", &[]);
        return log::info(
            "request",
            merge(about, json!({"request": body, "redactions": redactions})),
        )
        .map(|()| None);
    };
    let taken = shared.budget.borrow_mut().take(std::time::Instant::now());
    if let Err(why) = taken {
        return skip(settings, shared, &gathered, about, "budget", why)
            .await
            .map(|()| None);
    }
    let answer = match model.complete(&body).await {
        Ok(answer) => answer,
        Err(error) => {
            shared
                .metrics
                .inc("kausa_model_calls_total", &[("outcome", "error")]);
            if !error.reached {
                shared.budget.borrow_mut().refund();
            }
            if error.transient
                && let Some(retry) = retry
            {
                shared.metrics.inc("kausa_explanations_retried_total", &[]);
                log::warn(
                    "explanation retried",
                    merge(
                        about,
                        json!({
                            "reason": error.message,
                            "detail": error.detail,
                            "retry_in_seconds": RETRY_DELAY.as_secs(),
                        }),
                    ),
                )?;
                return Ok(Some(retry));
            }
            if let Some(detail) = &error.detail {
                log::warn(
                    "endpoint error",
                    merge(
                        about.clone(),
                        json!({"reason": error.message, "detail": detail}),
                    ),
                )?;
            }
            return skip(
                settings,
                shared,
                &gathered,
                about,
                "endpoint",
                &error.message,
            )
            .await
            .map(|()| None);
        }
    };
    if let Some(n) = answer.prompt_tokens {
        shared
            .metrics
            .add("kausa_model_tokens_total", &[("direction", "in")], n);
    }
    if let Some(n) = answer.completion_tokens {
        shared
            .metrics
            .add("kausa_model_tokens_total", &[("direction", "out")], n);
    }
    let explanation = match prompt::validate(&answer.content) {
        Ok(explanation) => explanation,
        Err(error) => {
            shared
                .metrics
                .inc("kausa_model_calls_total", &[("outcome", "invalid")]);
            let why = format!("invalid answer: {error}");
            return skip(settings, shared, &gathered, about, "invalid_answer", &why)
                .await
                .map(|()| None);
        }
    };
    shared
        .metrics
        .inc("kausa_model_calls_total", &[("outcome", "ok")]);
    let restored = explanation.map_clean(|text| redactor.restore(text));
    // The notification's text maps back only the namespace and the top-level owner.
    let narrow = settings.notify.is_some().then(|| {
        let incident = &gathered.incident;
        let names = [
            incident.key.namespace.as_str(),
            incident.owner.name.as_str(),
        ];
        explanation.map_clean(|text| redactor.restore_only(text, &names))
    });
    shared.dedup.borrow_mut().put(
        &fingerprint,
        restored.clone(),
        narrow.clone(),
        std::time::Instant::now(),
    );
    log::info(
        "explanation",
        merge(
            about,
            json!({
                "explanation": restored.to_json(),
                "prompt_version": PROMPT_VERSION,
                "tokens_in": answer.prompt_tokens,
                "tokens_out": answer.completion_tokens,
                "redactions": redactions,
            }),
        ),
    )?;
    emit(settings, shared, &gathered, Outcome::Explained(&restored)).await?;
    if let Some(narrow) = narrow {
        notify::enqueue(shared, notify::queued(&gathered.incident, narrow))?;
    }
    Ok(None)
}

/// Builds the request, cutting the context until it fits `max_input_tokens`: first the log tail
/// (halved each time), then all but the first Pod and three events. `None` when it still does not
/// fit.
fn fit(gathered: &mut Gathered, settings: &Settings) -> Option<(Value, crate::redact::Redactor)> {
    loop {
        let (context, redactor) = context::build(gathered);
        let body = prompt::request(
            &settings.model_name,
            &context,
            settings.mode,
            settings.max_tokens,
        );
        if estimate_tokens(&body) <= settings.max_input_tokens {
            return Some((body, redactor));
        }
        match gathered.log_tail.as_mut() {
            Some(log) if log.len() > 256 => {
                let mut start = log.len() / 2;
                while !log.is_char_boundary(start) {
                    start += 1;
                }
                *log = log[start..].to_owned();
            }
            Some(_) => gathered.log_tail = None,
            None if gathered.pods.len() > 1 || gathered.events.len() > 3 => {
                gathered.pods.truncate(1);
                gathered.events.truncate(3);
            }
            None => return None,
        }
    }
}

/// A rough count of the input tokens of a request: a quarter of its bytes, the usual rule of thumb
/// for English and JSON. Only a cap; the endpoint's own count is in the log and the metrics.
fn estimate_tokens(body: &Value) -> usize {
    body.to_string().len().div_ceil(4)
}

async fn skip(
    settings: &Settings,
    shared: &Shared<'_>,
    gathered: &Gathered,
    about: Value,
    reason: &str,
    why: &str,
) -> io::Result<()> {
    shared
        .metrics
        .inc("kausa_explanations_skipped_total", &[("reason", reason)]);
    log::warn("explanation skipped", merge(about, json!({"reason": why})))?;
    emit(settings, shared, gathered, Outcome::Skipped(why)).await
}

/// Writes an outcome: as an Event when Events are on, then, for an explanation, into the history
/// when it is on. Each step stands alone: a failure of one is logged and does not stop the other.
async fn emit(
    settings: &Settings,
    shared: &Shared<'_>,
    gathered: &Gathered,
    outcome: Outcome<'_>,
) -> io::Result<()> {
    if let Some(writer) = shared.writer.as_ref().filter(|_| settings.events) {
        write_event(writer, shared, gathered, &outcome).await?;
    }
    if let (Some(history), Outcome::Explained(explanation)) = (shared.history.as_ref(), &outcome) {
        history
            .write(
                &gathered.incident,
                explanation,
                &settings.model_name,
                shared.metrics,
            )
            .await?;
    }
    Ok(())
}

async fn write_event(
    writer: &Writer,
    shared: &Shared<'_>,
    gathered: &Gathered,
    outcome: &Outcome<'_>,
) -> io::Result<()> {
    let reason = match outcome {
        Outcome::Explained(_) => "Explained",
        Outcome::Skipped(_) => "ExplanationSkipped",
    };
    match writer
        .emit(&gathered.incident, outcome, Timestamp::now())
        .await
    {
        Ok(()) => {
            shared
                .metrics
                .inc("kausa_events_written_total", &[("reason", reason)]);
            Ok(())
        }
        Err(error) => {
            shared.metrics.inc("kausa_event_write_errors_total", &[]);
            log::warn(
                "event write failed",
                json!({"fingerprint": gathered.incident.fingerprint, "error": error}),
            )
        }
    }
}

async fn read_log(client: &Client, source: &LogSource) -> Result<String, String> {
    let params = LogParams {
        container: Some(source.container.clone()),
        previous: source.previous,
        tail_lines: Some(LOG_TAIL_LINES),
        limit_bytes: Some(LOG_LIMIT_BYTES),
        ..LogParams::default()
    };
    let api = Api::<Pod>::namespaced(client.clone(), &source.namespace);
    let read = async {
        let stream = api
            .log_stream(&source.pod, &params)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = std::pin::pin!(stream);
        let mut kept: Vec<u8> = Vec::new();
        let mut truncated = false;
        let mut buffer = [0u8; 8192];
        loop {
            let n = stream.read(&mut buffer).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&buffer[..n]);
            if kept.len() > LOG_KEEP_BYTES {
                kept.drain(..kept.len() - LOG_KEEP_BYTES);
                truncated = true;
            }
        }
        Ok::<_, String>(tail_text(&kept, truncated))
    };
    match tokio::time::timeout(LOG_TIMEOUT, read).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "no answer within {} seconds",
            LOG_TIMEOUT.as_secs()
        )),
    }
}

/// The kept end of a log as text: invalid UTF-8 replaced, and, when the start was cut, the first
/// partial line dropped.
fn tail_text(bytes: &[u8], truncated: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    match (truncated, text.find('\n')) {
        (true, Some(i)) => text[i + 1..].to_owned(),
        _ => text.into_owned(),
    }
}

fn counts(counts: &BTreeMap<&'static str, usize>) -> Value {
    Value::Object(
        counts
            .iter()
            .map(|(rule, n)| ((*rule).to_owned(), Value::from(*n)))
            .collect(),
    )
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Value::Object(a), Value::Object(b)) = (&mut a, b) {
        a.extend(b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_log_drops_its_partial_first_line_and_survives_invalid_utf8() {
        assert_eq!(tail_text(b"rt of a line\nlast line\n", true), "last line\n");
        assert_eq!(tail_text(b"whole\n", false), "whole\n");
        assert_eq!(tail_text(b"bad \xff byte\n", false), "bad \u{FFFD} byte\n");
    }

    // Acceptance tests for notifications (docs/adr/0015-notifications.md), written before the
    // code. They use `Settings::notify: Option<notify::Notifier>`, `Shared::notifications:
    // RefCell<VecDeque<notify::Queued>>` and the Dedup API of budget.rs (`put` with a notice,
    // `peek`, `mark_notified`). They run `run` against a fake model on localhost, with no log
    // read and no Event or history writer, so the Kubernetes client is never used.

    use std::cell::RefCell;
    use std::collections::{BTreeSet, VecDeque};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use crate::incident::{Incident, IncidentKey, Signal};
    use crate::model::ModelClient;
    use crate::notify::{Format, Notifier};
    use crate::prompt::Explanation;
    use crate::record::{ContainerRecord, OwnerRef, PodRecord};

    const FINGERPRINT: &str = "5d0c2b7e9a1f4c38";

    /// A fake model on localhost that answers one request with `content` as the first choice,
    /// or with `status` and an empty object when `content` is `None`. Returns its base URL.
    fn model(status: &'static str, content: Option<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}/v1", listener.local_addr().expect("an address"));
        std::thread::spawn(move || {
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
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let body = match content {
                Some(content) => json!({
                    "choices": [{"message": {"content": content}}],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 20},
                })
                .to_string(),
                None => "{}".to_owned(),
            };
            let answer = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(answer.as_bytes());
        });
        url
    }

    /// A base URL with nothing listening: a call to it would fail.
    fn nowhere() -> String {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        format!("http://127.0.0.1:{port}/v1")
    }

    /// The model's answer: every name a placeholder.
    fn answer() -> String {
        json!({
            "summary": "deployment-1 in namespace-1 crashes: pod-1 on node-1 exits.",
            "cause": "container-1 runs as serviceaccount-1.",
            "steps": ["Check Deployment-1's args."],
            "confidence": "medium",
        })
        .to_string()
    }

    fn narrow() -> Explanation {
        Explanation {
            summary: "web in shop crashes: pod-1 on node-1 exits.".into(),
            cause: "container-1 runs as serviceaccount-1.".into(),
            steps: vec!["Check web's args.".into()],
            confidence: "medium".into(),
        }
    }

    fn full() -> Explanation {
        Explanation {
            summary: "web in shop crashes: web-7d9c8b6f4d-2xkqp on worker-7 exits.".into(),
            cause: "server runs as web-runner.".into(),
            steps: vec!["Check web's args.".into()],
            confidence: "medium".into(),
        }
    }

    fn stored(summary: &str) -> Explanation {
        Explanation {
            summary: summary.into(),
            cause: "c".into(),
            steps: vec![],
            confidence: "low".into(),
        }
    }

    fn work() -> Work {
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
        let incident = Incident {
            sample: Signal {
                key: key.clone(),
                owner: owner.clone(),
                observed_at: first_seen,
                event: None,
                pod: Some("web-7d9c8b6f4d-2xkqp".into()),
                container: Some("server".into()),
                image: None,
                reason: "CrashLoopBackOff".into(),
                message: "SAMPLE-CANARY".into(),
                exit_code: Some(1),
            },
            key,
            owner,
            fingerprint: FINGERPRINT.into(),
            first_seen,
            last_seen: first_seen,
            pods: BTreeSet::from(["web-7d9c8b6f4d-2xkqp".to_owned()]),
        };
        Work {
            gathered: Gathered {
                incident,
                pods: vec![PodRecord {
                    namespace: "shop".into(),
                    name: "web-7d9c8b6f4d-2xkqp".into(),
                    uid: "uid-pod".into(),
                    controller: None,
                    phase: Some("Running".into()),
                    unschedulable: None,
                    unschedulable_reason: None,
                    reason: None,
                    message: None,
                    failed_at: None,
                    node_name: Some("worker-7".into()),
                    service_account: Some("web-runner".into()),
                    containers: vec![ContainerRecord {
                        name: "server".into(),
                        image: Some("busybox:1.37".into()),
                        restart_count: 4,
                        ..Default::default()
                    }],
                }],
                events: vec![],
                pods_total: 1,
                log_tail: Some("LOG-CANARY panic\n".into()),
            },
            log: None,
            attempt: 0,
        }
    }

    fn settings(model: Option<String>, notify: bool, calls_per_hour: usize) -> Settings {
        Settings {
            model: model.map(|url| ModelClient::new(&url, None, true).expect("a model client")),
            model_name: "m".into(),
            logs: false,
            mode: OutputMode::JsonSchema,
            max_tokens: 800,
            max_input_tokens: 6000,
            events: false,
            history_days: None,
            calls_per_hour,
            calls_per_day: 200,
            dedup_ttl: Duration::from_secs(6 * 3600),
            instance: "kausa".into(),
            notify: notify.then(|| {
                let url = reqwest::Url::parse(&format!("{}/hook", nowhere())).expect("a URL");
                Notifier::new(url, Format::Json, None, true).expect("a notifier")
            }),
        }
    }

    fn shared<'a>(settings: &Settings, metrics: &'a Metrics) -> Shared<'a> {
        Shared {
            budget: RefCell::new(Budget::new(settings.calls_per_hour, settings.calls_per_day)),
            dedup: RefCell::new(Dedup::new(settings.dedup_ttl, 1024)),
            writer: None,
            history: None,
            metrics,
            notifications: RefCell::new(VecDeque::new()),
        }
    }

    /// Runs one explanation and returns what `run` returned (whether a retry was asked).
    fn explain(job: Work, settings: &Settings, shared: &Shared<'_>) -> bool {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(async {
                let config = kube::Config::new("http://127.0.0.1:9".parse().expect("a URL"));
                let client = Client::try_from(config).expect("a client");
                run(job, client, settings, shared).await
            })
            .expect("the log")
            .is_some()
    }

    fn queued_summaries(shared: &Shared<'_>) -> Vec<String> {
        shared
            .notifications
            .borrow()
            .iter()
            .map(|q| q.explanation.summary.clone())
            .collect()
    }

    #[test]
    fn a_new_explanation_queues_its_narrow_restore_and_keeps_the_full_one() {
        let settings = settings(Some(model("200 OK", Some(answer()))), true, 30);
        let metrics = Metrics::default();
        let shared = shared(&settings, &metrics);
        assert!(!explain(work(), &settings, &shared));
        {
            let queue = shared.notifications.borrow();
            assert_eq!(queue.len(), 1);
            let item = &queue[0];
            assert_eq!(item.fingerprint, FINGERPRINT);
            assert_eq!(item.explanation, narrow());
            assert_eq!(item.notice.namespace, "shop");
            assert_eq!(item.notice.name, "web");
            assert_eq!(item.notice.summary, narrow().summary);
            assert_eq!(item.notice.first_seen, "2026-10-28T11:59:00Z");
        }
        // The Event and the log keep the full restore; the entry stores the narrow one too.
        let dedup = shared.dedup.borrow();
        let entry = dedup.peek(FINGERPRINT).expect("an entry");
        assert_eq!(entry.explanation, full());
        assert_eq!(entry.notice, Some(narrow()));
        assert!(!entry.notified);
    }

    #[test]
    fn without_notifications_nothing_is_queued_and_no_notice_is_stored() {
        let settings = settings(Some(model("200 OK", Some(answer()))), false, 30);
        let metrics = Metrics::default();
        let shared = shared(&settings, &metrics);
        assert!(!explain(work(), &settings, &shared));
        assert!(shared.notifications.borrow().is_empty());
        let dedup = shared.dedup.borrow();
        let entry = dedup.peek(FINGERPRINT).expect("an entry");
        assert_eq!(entry.explanation, full());
        assert_eq!(entry.notice, None);
    }

    #[test]
    fn a_cached_repeat_queues_the_stored_text_only_while_it_is_not_notified() {
        // The model is never called on the cached path.
        let settings = settings(Some(nowhere()), true, 30);
        let metrics = Metrics::default();
        let shared = shared(&settings, &metrics);
        shared.dedup.borrow_mut().put(
            FINGERPRINT,
            stored("full text"),
            Some(stored("notification text")),
            std::time::Instant::now(),
        );
        assert!(!explain(work(), &settings, &shared));
        assert_eq!(queued_summaries(&shared), ["notification text"]);
        // A failed or pending send: the next occurrence queues it again.
        assert!(!explain(work(), &settings, &shared));
        assert_eq!(
            queued_summaries(&shared),
            ["notification text", "notification text"]
        );
        // Once sent, a repeat queues nothing.
        assert!(
            shared
                .dedup
                .borrow_mut()
                .mark_notified(FINGERPRINT, &stored("notification text"))
        );
        assert!(!explain(work(), &settings, &shared));
        assert_eq!(queued_summaries(&shared).len(), 2);
        assert!(
            metrics
                .render()
                .contains("kausa_explanations_cached_total 3\n")
        );
    }

    #[test]
    fn a_cached_entry_without_a_notice_or_with_notifications_off_queues_nothing() {
        for (notify, notice) in [(true, None), (false, Some(stored("notification text")))] {
            let settings = settings(Some(nowhere()), notify, 30);
            let metrics = Metrics::default();
            let shared = shared(&settings, &metrics);
            shared.dedup.borrow_mut().put(
                FINGERPRINT,
                stored("full text"),
                notice,
                std::time::Instant::now(),
            );
            assert!(!explain(work(), &settings, &shared));
            assert!(shared.notifications.borrow().is_empty(), "notify {notify}");
        }
    }

    #[test]
    fn a_skipped_explanation_queues_nothing() {
        // The budget is spent.
        let settings_budget = settings(Some(nowhere()), true, 0);
        // The answer is not an explanation.
        let settings_invalid = settings(
            Some(model("200 OK", Some("not an explanation".into()))),
            true,
            30,
        );
        // The endpoint refuses for good.
        let settings_refused = settings(Some(model("400 Bad Request", None)), true, 30);
        for (case, settings) in [
            ("budget", settings_budget),
            ("invalid", settings_invalid),
            ("refused", settings_refused),
        ] {
            let metrics = Metrics::default();
            let shared = shared(&settings, &metrics);
            assert!(!explain(work(), &settings, &shared), "{case}");
            assert!(shared.notifications.borrow().is_empty(), "{case}");
            assert!(
                metrics
                    .render()
                    .contains("kausa_explanations_skipped_total{reason="),
                "{case}"
            );
        }
    }

    #[test]
    fn a_retried_call_queues_nothing() {
        let settings = settings(Some(model("503 Service Unavailable", None)), true, 30);
        let metrics = Metrics::default();
        let shared = shared(&settings, &metrics);
        assert!(explain(work(), &settings, &shared), "a retry is asked");
        assert!(shared.notifications.borrow().is_empty());
    }

    #[test]
    fn a_dry_run_queues_nothing() {
        let settings = settings(None, true, 30);
        let metrics = Metrics::default();
        let shared = shared(&settings, &metrics);
        assert!(!explain(work(), &settings, &shared));
        assert!(shared.notifications.borrow().is_empty());
        assert!(metrics.render().contains("kausa_requests_logged_total 1\n"));
    }
}
