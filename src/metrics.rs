//! Prometheus metrics and health endpoints (docs/reference/command-line.md, "Metrics").
//!
//! Counters in memory, rendered in the text exposition format, and a minimal HTTP/1.1 responder
//! for `GET /metrics`, `/healthz` and `/readyz`: kausa serves nothing else, so it needs no HTTP
//! framework. Each connection is answered and closed, with a short timeout.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The counters kausa exports, with their help text.
pub const COUNTERS: [(&str, &str); 12] = [
    (
        "kausa_incidents_total",
        "Incidents that settled, by detection.",
    ),
    (
        "kausa_model_calls_total",
        "Calls to the model endpoint, by outcome (ok, error, invalid).",
    ),
    (
        "kausa_model_tokens_total",
        "Tokens the endpoint reported, by direction (in, out).",
    ),
    (
        "kausa_explanations_cached_total",
        "Incidents explained again from the deduplication cache.",
    ),
    (
        "kausa_explanations_skipped_total",
        "Incidents not explained, by reason.",
    ),
    (
        "kausa_explanations_retried_total",
        "Calls tried again after a transient error.",
    ),
    (
        "kausa_explanations_dropped_total",
        "Incidents dropped because the explanation queue was full.",
    ),
    (
        "kausa_requests_logged_total",
        "Requests logged instead of sent (--dry-run).",
    ),
    (
        "kausa_redactions_total",
        "Values replaced by redaction, by rule.",
    ),
    (
        "kausa_events_written_total",
        "Events created or updated, by reason.",
    ),
    (
        "kausa_event_write_errors_total",
        "Events that could not be written.",
    ),
    (
        "kausa_watch_errors_total",
        "Errors of the Pod and Event watches, which retry with backoff, by watch.",
    ),
];

#[derive(Debug, Default)]
pub struct Metrics {
    counters: RefCell<BTreeMap<(&'static str, String), u64>>,
    ready: Cell<bool>,
}

impl Metrics {
    /// Adds `by` to the counter `name` with the given labels.
    pub fn add(&self, name: &'static str, labels: &[(&str, &str)], by: u64) {
        let labels = labels
            .iter()
            .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
            .collect::<Vec<_>>()
            .join(",");
        *self
            .counters
            .borrow_mut()
            .entry((name, labels))
            .or_insert(0) += by;
    }

    pub fn inc(&self, name: &'static str, labels: &[(&str, &str)]) {
        self.add(name, labels, 1);
    }

    /// Marks kausa ready: every watch has listed once.
    pub fn set_ready(&self) {
        self.ready.set(true);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.get()
    }

    /// The text exposition format.
    pub fn render(&self) -> String {
        let counters = self.counters.borrow();
        let mut out = String::new();
        for (name, help) in COUNTERS {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n"));
            let mut any = false;
            for ((n, labels), value) in counters.iter() {
                if *n != name {
                    continue;
                }
                any = true;
                if labels.is_empty() {
                    out.push_str(&format!("{name} {value}\n"));
                } else {
                    out.push_str(&format!("{name}{{{labels}}} {value}\n"));
                }
            }
            if !any {
                out.push_str(&format!("{name} 0\n"));
            }
        }
        out
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Answers one connection: the first line of the request decides the response.
pub async fn answer(mut stream: TcpStream, metrics: &Metrics) {
    let mut buffer = [0u8; 1024];
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer)).await;
    let Ok(Ok(n)) = read else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..n]);
    let first = request.lines().next().unwrap_or_default();
    let (status, content_type, body) = respond(first, metrics);
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        stream.write_all(response.as_bytes()),
    )
    .await;
    let _ = stream.shutdown().await;
}

/// The status, content type and body for a request line.
pub fn respond(request_line: &str, metrics: &Metrics) -> (&'static str, &'static str, String) {
    let mut parts = request_line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let path = path.split('?').next().unwrap_or_default();
    match (method, path) {
        ("GET", "/metrics") => ("200 OK", "text/plain; version=0.0.4", metrics.render()),
        ("GET", "/healthz") => ("200 OK", "text/plain", "ok\n".into()),
        ("GET", "/readyz") if metrics.is_ready() => ("200 OK", "text/plain", "ready\n".into()),
        ("GET", "/readyz") => ("503 Service Unavailable", "text/plain", "starting\n".into()),
        ("GET", _) => ("404 Not Found", "text/plain", "not found\n".into()),
        _ => (
            "405 Method Not Allowed",
            "text/plain",
            "method not allowed\n".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_render_in_the_exposition_format() {
        let m = Metrics::default();
        m.inc("kausa_incidents_total", &[("detection", "D001")]);
        m.inc("kausa_incidents_total", &[("detection", "D001")]);
        m.add("kausa_model_tokens_total", &[("direction", "in")], 500);
        let text = m.render();
        assert!(text.contains("# TYPE kausa_incidents_total counter\n"));
        assert!(
            text.contains("kausa_incidents_total{detection=\"D001\"} 2\n"),
            "{text}"
        );
        assert!(text.contains("kausa_model_tokens_total{direction=\"in\"} 500\n"));
        assert!(text.contains("kausa_events_written_total 0\n"));
    }

    #[test]
    fn label_values_are_escaped() {
        let m = Metrics::default();
        m.inc(
            "kausa_explanations_skipped_total",
            &[("reason", "a\"b\\c\nd")],
        );
        assert!(m.render().contains(r#"reason="a\"b\\c\nd""#));
    }

    #[test]
    fn health_and_readiness() {
        let m = Metrics::default();
        assert_eq!(respond("GET /healthz HTTP/1.1", &m).0, "200 OK");
        assert_eq!(
            respond("GET /readyz HTTP/1.1", &m).0,
            "503 Service Unavailable"
        );
        m.set_ready();
        assert_eq!(respond("GET /readyz HTTP/1.1", &m).0, "200 OK");
        assert_eq!(respond("GET /metrics?x=1 HTTP/1.1", &m).0, "200 OK");
        assert_eq!(respond("GET /secrets HTTP/1.1", &m).0, "404 Not Found");
        assert_eq!(
            respond("POST /metrics HTTP/1.1", &m).0,
            "405 Method Not Allowed"
        );
    }
}
