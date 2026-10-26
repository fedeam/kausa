//! From an incident to an explanation: read the log tail, build the redacted context, then either
//! log the request (`--dry-run`) or send it to the model and validate the answer.
//!
//! One explanation runs at a time, and the queue in front of it is bounded, so neither a slow
//! endpoint nor a burst of incidents can stop the watches or grow kausa's memory.

use std::cell::RefCell;
use std::collections::BTreeMap;
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
use crate::log;
use crate::metrics::Metrics;
use crate::model::ModelClient;
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
    /// Model calls allowed per hour and per day.
    pub calls_per_hour: usize,
    pub calls_per_day: usize,
    /// How long an explanation is reused for the same incident fingerprint.
    pub dedup_ttl: Duration,
    /// The reporting instance of kausa's Events: its Pod's name, or `kausa`.
    pub instance: String,
}

/// What the explanations share across incidents: the budget, the deduplication cache, the Event
/// writer and the metrics. Borrowed only between awaits.
pub struct Shared<'a> {
    pub budget: RefCell<Budget>,
    pub dedup: RefCell<Dedup>,
    pub writer: Option<Writer>,
    pub metrics: &'a Metrics,
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
pub struct Work {
    pub gathered: Gathered,
    pub log: Option<LogSource>,
}

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
/// explanations are written as Events when that is on.
pub async fn run(
    job: Work,
    client: Client,
    settings: &Settings,
    shared: &Shared<'_>,
) -> io::Result<()> {
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
        if let Some(explanation) = cached {
            shared.metrics.inc("kausa_explanations_cached_total", &[]);
            log::info(
                "explanation",
                merge(
                    about,
                    json!({"explanation": explanation.to_json(), "cached": true}),
                ),
            )?;
            return emit(
                settings,
                shared,
                &gathered,
                Outcome::Explained(&explanation),
            )
            .await;
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
        .await;
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
        );
    };
    let taken = shared.budget.borrow_mut().take(std::time::Instant::now());
    if let Err(why) = taken {
        return skip(settings, shared, &gathered, about, "budget", why).await;
    }
    let answer = match model.complete(&body).await {
        Ok(answer) => answer,
        Err(error) => {
            shared
                .metrics
                .inc("kausa_model_calls_total", &[("outcome", "error")]);
            return skip(settings, shared, &gathered, about, "endpoint", &error).await;
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
            return skip(settings, shared, &gathered, about, "invalid_answer", &why).await;
        }
    };
    shared
        .metrics
        .inc("kausa_model_calls_total", &[("outcome", "ok")]);
    let restored = explanation.map_clean(|text| redactor.restore(text));
    shared
        .dedup
        .borrow_mut()
        .put(&fingerprint, restored.clone(), std::time::Instant::now());
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
    emit(settings, shared, &gathered, Outcome::Explained(&restored)).await
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

async fn emit(
    settings: &Settings,
    shared: &Shared<'_>,
    gathered: &Gathered,
    outcome: Outcome<'_>,
) -> io::Result<()> {
    let Some(writer) = shared.writer.as_ref().filter(|_| settings.events) else {
        return Ok(());
    };
    let reason = match outcome {
        Outcome::Explained(_) => "Explained",
        Outcome::Skipped(_) => "ExplanationSkipped",
    };
    match writer
        .emit(&gathered.incident, &outcome, Timestamp::now())
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
}
