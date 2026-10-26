//! From an incident to an explanation: read the log tail, build the redacted context, then either
//! log the request (`--dry-run`) or send it to the model and validate the answer.
//!
//! One explanation runs at a time, and the queue in front of it is bounded, so neither a slow
//! endpoint nor a burst of incidents can stop the watches or grow kausa's memory.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use futures::AsyncReadExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::LogParams;
use kube::{Api, Client};
use serde_json::{Value, json};

use crate::context::{self, Gathered};
use crate::detect::Detection;
use crate::log;
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

/// Reads the log tail, builds the request and explains it, then logs the outcome.
pub async fn run(job: Work, client: Client, settings: &Settings) -> io::Result<()> {
    let mut gathered = job.gathered;
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
    let (context, redactor) = context::build(&gathered);
    let body = prompt::request(
        &settings.model_name,
        &context,
        settings.mode,
        settings.max_tokens,
    );
    let incident = &gathered.incident;
    let about = json!({
        "detection": incident.key.detection.id(),
        "namespace": incident.key.namespace,
        "owner": {"kind": incident.owner.kind, "name": incident.owner.name},
        "fingerprint": incident.fingerprint,
    });
    let redactions = counts(redactor.counts());
    let Some(model) = &settings.model else {
        // Dry-run: the exact body that would be sent, which holds only redacted data.
        return log::info(
            "request",
            merge(about, json!({"request": body, "redactions": redactions})),
        );
    };
    match model.complete(&body).await {
        Err(error) => log::warn(
            "explanation skipped",
            merge(about, json!({"reason": error})),
        ),
        Ok(answer) => match prompt::validate(&answer.content) {
            Err(error) => log::warn(
                "explanation skipped",
                merge(about, json!({"reason": format!("invalid answer: {error}")})),
            ),
            Ok(explanation) => {
                let restored = explanation.map_clean(|text| redactor.restore(text));
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
                )
            }
        },
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
