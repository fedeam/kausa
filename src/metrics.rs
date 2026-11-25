//! Prometheus metrics and health endpoints (docs/reference/command-line.md, "Metrics").
//!
//! Counters, gauges and one histogram in memory, rendered in the text exposition format, and a
//! minimal HTTP/1.1 responder for `GET /metrics`, `/healthz` and `/readyz`: kausa serves nothing
//! else, so it needs no HTTP framework. Each connection is answered and closed, with a short
//! timeout. The request line is read to its end first: a request may arrive in pieces.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The counters kausa exports, with their help text.
pub const COUNTERS: [(&str, &str); 25] = [
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
        "Values replaced by redaction in the requests, by rule.",
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
        "Errors of the Pod, Event and Deployment watches, which retry with backoff, by watch.",
    ),
    (
        "kausa_history_writes_total",
        "Explanation objects written, by outcome (created, updated, refused, conflict, error).",
    ),
    (
        "kausa_history_deleted_total",
        "Explanation objects deleted by the sweep, by reason (age, cap).",
    ),
    (
        "kausa_history_sweep_errors_total",
        "Failed list or delete calls of the history sweep.",
    ),
    (
        "kausa_notifications_total",
        "Notifications by final outcome (sent, failed, dropped, capped).",
    ),
    (
        "kausa_notifications_retried_total",
        "Notifications tried again after a transient error.",
    ),
    (
        "kausa_leadership_changes_total",
        "Times this process became leader or stepped down to standby, by `to` (leader, standby).",
    ),
    (
        "kausa_lease_errors_total",
        "Failed calls on the Lease, by `call` (get, patch); conflicts are not errors.",
    ),
    (
        "kausa_answer_redactions_total",
        "Values replaced by redaction in the model's answers, by rule.",
    ),
    (
        "kausa_watch_relists_total",
        "Lists of the Pod, Event and Deployment watches after the first of each watch, by watch.",
    ),
    (
        "kausa_watch_restarts_total",
        "New watchers started after the API server refused a watch (403), by watch.",
    ),
    // docs/adr/0025-withhold-destructive-commands.md: once per fresh answer whose restored
    // version carries the notice; a cached repeat counts nothing.
    (
        "kausa_answers_withheld_total",
        "Model answers in which kausa replaced text with its withheld-command notice.",
    ),
    // docs/adr/0027-fact-statement-for-the-detections.md, point 8.
    (
        "kausa_facts_dropped_total",
        "Facts dropped because the fact queue was full.",
    ),
    (
        "kausa_fact_reasons_omitted_total",
        "Fact statements that left out a reason outside the list of known reasons.",
    ),
];

/// The counters of the Incident objects (docs/adr/0028-incident-resource-and-remediation-report.md,
/// points 1 and 4), rendered after the other counters and, unlike them, only once a series was
/// set: `kausa run` sets each at 0 at start with incidents on, so that with incidents off the
/// exposition has no line of them.
pub const INCIDENT_COUNTERS: [(&str, &str); 4] = [
    (
        "kausa_incidents_refused_total",
        "Incident objects not created, by reason (table-full, schema, starting).",
    ),
    (
        "kausa_incident_write_errors_total",
        "Incident objects that could not be created.",
    ),
    (
        "kausa_incidents_deleted_total",
        "Incident objects deleted by the sweep, by reason (age, cap).",
    ),
    (
        "kausa_incident_sweep_errors_total",
        "Failed list or delete calls of the incident sweep.",
    ),
];

/// The gauges kausa exports, with their help text, rendered after the counters
/// (docs/adr/0019-high-availability.md). `kausa_leader` is always rendered; the budget and queue
/// gauges (docs/adr/0021-refill-the-call-budget.md) and the endpoint gauges
/// (docs/adr/0020-check-the-endpoint.md) only once a series was set, so that a process that has
/// no budget, no queue or no endpoint shows no 0 an alert could fire on. So are the build, record
/// and cache gauges, which `kausa run` sets at start (the deduplication cache only with a model).
pub const GAUGES: [(&str, &str); 9] = [
    (
        "kausa_leader",
        "1 while this process acts (it holds the Lease, or runs without --leader-lease), 0 as a \
         standby.",
    ),
    (
        "kausa_budget_calls_remaining",
        "Model calls this process may start now, by window (hour: the refill bucket within the \
         60-minute cap; day: the 24-hour cap).",
    ),
    (
        "kausa_explanation_queue_length",
        "Incidents waiting for an explanation (at most 16).",
    ),
    (
        "kausa_endpoint_state",
        "State of the model endpoint's breaker: 1 for the current state (closed, open, \
         half_open), 0 for the others; closed on a standby.",
    ),
    (
        "kausa_model_last_success_timestamp_seconds",
        "Unix time of the last call answered with a chat completion, 0 before the first.",
    ),
    (
        "kausa_build_info",
        "Always 1; the labels name this build of kausa (version, prompt_version).",
    ),
    (
        "kausa_records",
        "Records kausa holds, by kind (pods, events, deployments); 0 on a standby.",
    ),
    (
        "kausa_owner_cache_entries",
        "Owner lookups in the cache, expired ones included until the next lookup that reads the \
         API server; 0 on a standby.",
    ),
    (
        "kausa_dedup_cache_entries",
        "Explanations in the deduplication cache (at most 1024), expired ones included until \
         read again or evicted.",
    ),
];

/// The upper bounds, in seconds, of the buckets of `kausa_model_call_duration_seconds`.
pub const CALL_DURATION_BUCKETS: [f64; 11] = [
    0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0,
];

/// The histograms kausa exports, with their help text and bucket bounds, rendered after the
/// gauges and only once observed.
pub const HISTOGRAMS: [(&str, &str, &[f64]); 1] = [(
    "kausa_model_call_duration_seconds",
    "Duration of the calls to the model endpoint, by outcome (ok, error, invalid).",
    &CALL_DURATION_BUCKETS,
)];

/// One series of a histogram: the observations in each bucket (not cumulative; the values past
/// the last bound are in `count` only), their sum and their number.
#[derive(Debug, Default)]
struct Histogram {
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

#[derive(Debug)]
pub struct Metrics {
    counters: RefCell<BTreeMap<(&'static str, String), u64>>,
    /// The series of every gauge but `kausa_leader`, by name and labels; set, never added to.
    gauges: RefCell<BTreeMap<(&'static str, String), u64>>,
    /// The series of the histograms, by name and labels.
    histograms: RefCell<BTreeMap<(&'static str, String), Histogram>>,
    /// Set once, never cleared (docs/adr/0013-namespaced-install-mode.md).
    ready: Cell<bool>,
    /// Whether this process acts: it holds the Lease, or runs without --leader-lease.
    leading: Cell<bool>,
}

impl Default for Metrics {
    /// Ready later, and leading: without --leader-lease, kausa always acts.
    fn default() -> Self {
        Self {
            counters: RefCell::default(),
            gauges: RefCell::default(),
            histograms: RefCell::default(),
            ready: Cell::new(false),
            leading: Cell::new(true),
        }
    }
}

impl Metrics {
    /// Adds `by` to the counter `name` with the given labels.
    pub fn add(&self, name: &'static str, labels: &[(&str, &str)], by: u64) {
        *self
            .counters
            .borrow_mut()
            .entry((name, label_set(labels)))
            .or_insert(0) += by;
    }

    /// Sets the series of the gauge `name` with the given labels to `value`.
    pub fn set_gauge(&self, name: &'static str, labels: &[(&str, &str)], value: u64) {
        self.gauges
            .borrow_mut()
            .insert((name, label_set(labels)), value);
    }

    /// Observes `value` in the histogram `name` with the given labels: the first bucket whose
    /// bound is at least `value` counts it (none: only `+Inf`). An unknown name is a bug.
    pub fn observe(&self, name: &'static str, labels: &[(&str, &str)], value: f64) {
        let Some((_, _, bounds)) = HISTOGRAMS.iter().find(|(n, _, _)| *n == name) else {
            debug_assert!(false, "unknown histogram {name}");
            return;
        };
        let mut histograms = self.histograms.borrow_mut();
        let series = histograms
            .entry((name, label_set(labels)))
            .or_insert_with(|| Histogram {
                counts: vec![0; bounds.len()],
                ..Histogram::default()
            });
        if let Some(bucket) = bounds.iter().position(|bound| value <= *bound) {
            series.counts[bucket] += 1;
        }
        series.sum += value;
        series.count += 1;
    }

    pub fn inc(&self, name: &'static str, labels: &[(&str, &str)]) {
        self.add(name, labels, 1);
    }

    /// The value of the counter series `name` with the given labels, 0 when never set.
    #[cfg(test)]
    pub fn counter(&self, name: &'static str, labels: &[(&str, &str)]) -> u64 {
        let counters = self.counters.borrow();
        counters
            .get(&(name, label_set(labels)))
            .copied()
            .unwrap_or(0)
    }

    /// Marks kausa ready: every watch has listed once.
    pub fn set_ready(&self) {
        self.ready.set(true);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.get()
    }

    /// Sets the `kausa_leader` gauge and the `/readyz` body: `true` while this process acts,
    /// `false` as a standby. It never changes whether kausa is Ready.
    pub fn set_leading(&self, leading: bool) {
        self.leading.set(leading);
    }

    pub fn is_leading(&self) -> bool {
        self.leading.get()
    }

    /// The text exposition format.
    pub fn render(&self) -> String {
        let counters = self.counters.borrow();
        let mut out = String::new();
        for (name, help) in COUNTERS {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n"));
            if !push_series(&mut out, name, &counters) {
                out.push_str(&format!("{name} 0\n"));
            }
        }
        for (name, help) in INCIDENT_COUNTERS {
            // A counter never set is not rendered at all.
            let mut series = String::new();
            if push_series(&mut series, name, &counters) {
                out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n"));
                out.push_str(&series);
            }
        }
        let gauges = self.gauges.borrow();
        for (name, help) in GAUGES {
            if name == "kausa_leader" {
                let value = u8::from(self.is_leading());
                out.push_str(&format!(
                    "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
                ));
                continue;
            }
            // A gauge never set is not rendered at all.
            let mut series = String::new();
            if push_series(&mut series, name, &gauges) {
                out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n"));
                out.push_str(&series);
            }
        }
        let histograms = self.histograms.borrow();
        for (name, help, bounds) in HISTOGRAMS {
            let mut any = false;
            for ((n, labels), series) in histograms.iter() {
                if *n != name {
                    continue;
                }
                if !any {
                    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} histogram\n"));
                    any = true;
                }
                let sep = if labels.is_empty() { "" } else { "," };
                let mut cumulative = 0;
                for (bound, n) in bounds.iter().zip(&series.counts) {
                    cumulative += n;
                    out.push_str(&format!(
                        "{name}_bucket{{{labels}{sep}le=\"{bound}\"}} {cumulative}\n"
                    ));
                }
                let (count, sum) = (series.count, series.sum);
                out.push_str(&format!(
                    "{name}_bucket{{{labels}{sep}le=\"+Inf\"}} {count}\n"
                ));
                if labels.is_empty() {
                    out.push_str(&format!("{name}_sum {sum}\n{name}_count {count}\n"));
                } else {
                    out.push_str(&format!(
                        "{name}_sum{{{labels}}} {sum}\n{name}_count{{{labels}}} {count}\n"
                    ));
                }
            }
        }
        out
    }
}

/// The labels of a series, formatted and escaped: `k1="v1",k2="v2"`.
fn label_set(labels: &[(&str, &str)]) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Appends the series of `name`, sorted by labels; says whether there was one.
fn push_series(
    out: &mut String,
    name: &str,
    series: &BTreeMap<(&'static str, String), u64>,
) -> bool {
    let mut any = false;
    for ((n, labels), value) in series {
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
    any
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// How long a client has to send its request line, and how long kausa waits to write the answer.
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Answers one connection: the first line of the request decides the response. A client that
/// does not end that line within `IO_TIMEOUT` gets no answer.
pub async fn answer(mut stream: TcpStream, metrics: &Metrics) {
    let mut buffer = [0u8; 1024];
    let read = tokio::time::timeout(IO_TIMEOUT, request_line(&mut stream, &mut buffer)).await;
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
    let _ = tokio::time::timeout(IO_TIMEOUT, stream.write_all(response.as_bytes())).await;
    let _ = stream.shutdown().await;
}

/// Reads into `buffer` until the request line has its end, the client stops sending or the
/// buffer is full, and returns how many bytes it read. One read is not enough: the line may
/// arrive in pieces, and a proxy that takes over an HTTP `CONNECT` can pass on the first byte of
/// a request on its own, which would read as the method `G`.
async fn request_line(stream: &mut TcpStream, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let n = stream.read(&mut buffer[filled..]).await?;
        filled += n;
        if n == 0 || buffer[filled - n..filled].contains(&b'\n') {
            break;
        }
    }
    Ok(filled)
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
        ("GET", "/readyz") if metrics.is_ready() && metrics.is_leading() => {
            ("200 OK", "text/plain", "ready\n".into())
        }
        ("GET", "/readyz") if metrics.is_ready() => ("200 OK", "text/plain", "standby\n".into()),
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

    // Acceptance tests for the explanation history (docs/adr/0014-explanation-history.md),
    // written before the code: three more counters in `COUNTERS`.
    #[test]
    fn the_history_counters_are_exported_with_their_help() {
        for counter in [
            (
                "kausa_history_writes_total",
                "Explanation objects written, by outcome (created, updated, refused, conflict, error).",
            ),
            (
                "kausa_history_deleted_total",
                "Explanation objects deleted by the sweep, by reason (age, cap).",
            ),
            (
                "kausa_history_sweep_errors_total",
                "Failed list or delete calls of the history sweep.",
            ),
        ] {
            assert!(COUNTERS.contains(&counter), "{counter:?}");
        }
        let names: std::collections::BTreeSet<&str> = COUNTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), COUNTERS.len(), "a counter is listed twice");
    }

    #[test]
    fn the_history_counters_render_as_zero_when_unused() {
        let m = Metrics::default();
        let text = m.render();
        for name in [
            "kausa_history_writes_total",
            "kausa_history_deleted_total",
            "kausa_history_sweep_errors_total",
        ] {
            assert!(
                text.contains(&format!("# TYPE {name} counter\n{name} 0\n")),
                "{name}: {text}"
            );
        }
        m.inc("kausa_history_writes_total", &[("outcome", "created")]);
        m.inc("kausa_history_writes_total", &[("outcome", "conflict")]);
        m.add("kausa_history_deleted_total", &[("reason", "age")], 3);
        m.inc("kausa_history_sweep_errors_total", &[]);
        let text = m.render();
        assert!(
            text.contains("kausa_history_writes_total{outcome=\"created\"} 1\n"),
            "{text}"
        );
        assert!(
            text.contains("kausa_history_writes_total{outcome=\"conflict\"} 1\n"),
            "{text}"
        );
        assert!(
            text.contains("kausa_history_deleted_total{reason=\"age\"} 3\n"),
            "{text}"
        );
        assert!(
            text.contains("kausa_history_sweep_errors_total 1\n"),
            "{text}"
        );
    }

    // Acceptance tests for notifications (docs/adr/0015-notifications.md), written before the
    // code: two more counters, appended to `COUNTERS`.
    #[test]
    fn the_notification_counters_come_last_with_their_help() {
        // The two counters of docs/adr/0019-high-availability.md, the one of
        // docs/adr/0022-redact-the-answer.md, the two watch counters of v0.6's
        // self-observability and the withheld answers of
        // docs/adr/0025-withhold-destructive-commands.md come after these.
        assert_eq!(COUNTERS.len(), 25);
        assert_eq!(
            COUNTERS[15],
            (
                "kausa_notifications_total",
                "Notifications by final outcome (sent, failed, dropped, capped)."
            )
        );
        assert_eq!(
            COUNTERS[16],
            (
                "kausa_notifications_retried_total",
                "Notifications tried again after a transient error."
            )
        );
        let names: std::collections::BTreeSet<&str> = COUNTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), COUNTERS.len(), "a counter is listed twice");
    }

    #[test]
    fn the_notification_counters_render_as_zero_when_unused() {
        let m = Metrics::default();
        let text = m.render();
        for name in [
            "kausa_notifications_total",
            "kausa_notifications_retried_total",
        ] {
            assert!(
                text.contains(&format!("# TYPE {name} counter\n{name} 0\n")),
                "{name}: {text}"
            );
        }
        m.inc("kausa_notifications_total", &[("outcome", "sent")]);
        m.inc("kausa_notifications_total", &[("outcome", "failed")]);
        m.inc("kausa_notifications_total", &[("outcome", "dropped")]);
        m.inc("kausa_notifications_total", &[("outcome", "capped")]);
        m.inc("kausa_notifications_retried_total", &[]);
        let text = respond("GET /metrics HTTP/1.1", &m).2;
        for line in [
            "kausa_notifications_total{outcome=\"sent\"} 1\n",
            "kausa_notifications_total{outcome=\"failed\"} 1\n",
            "kausa_notifications_total{outcome=\"dropped\"} 1\n",
            "kausa_notifications_total{outcome=\"capped\"} 1\n",
            "kausa_notifications_retried_total 1\n",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
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

    // Acceptance test for `D014` (docs/adr/0018-stuck-rollouts.md), written before the code: the
    // Deployment watch adds no counter; its errors are counted with the other watches', under
    // `watch="deployments"`, and `D014` incidents under `kausa_incidents_total`.
    #[test]
    fn the_deployment_watch_adds_no_counter() {
        // 23: the standby's two counters (docs/adr/0019-high-availability.md), the answer's
        // redactions (docs/adr/0022-redact-the-answer.md), the watch relists and restarts of
        // v0.6's self-observability and the withheld answers of
        // docs/adr/0025-withhold-destructive-commands.md came later.
        assert_eq!(COUNTERS.len(), 25, "a counter was added");
        assert!(COUNTERS.contains(&(
            "kausa_watch_errors_total",
            "Errors of the Pod, Event and Deployment watches, which retry with backoff, by watch."
        )));
        let m = Metrics::default();
        m.inc("kausa_watch_errors_total", &[("watch", "deployments")]);
        m.inc("kausa_incidents_total", &[("detection", "D014")]);
        let text = m.render();
        assert!(
            text.contains("kausa_watch_errors_total{watch=\"deployments\"} 1\n"),
            "{text}"
        );
        assert!(
            text.contains("kausa_incidents_total{detection=\"D014\"} 1\n"),
            "{text}"
        );
    }

    // Acceptance tests for the standby replica (docs/adr/0019-high-availability.md), written before
    // the code: a gauge after the counters, two counters appended, and `/readyz` saying whether
    // this replica acts. No label names a Pod, a namespace or the Lease.

    const LEADER_HELP: &str = "1 while this process acts (it holds the Lease, or runs without \
                               --leader-lease), 0 as a standby.";

    #[test]
    fn the_standby_counters_come_last_with_their_help() {
        // The counter of docs/adr/0022-redact-the-answer.md, the two watch counters of v0.6's
        // self-observability and the withheld answers of
        // docs/adr/0025-withhold-destructive-commands.md come after these.
        assert_eq!(COUNTERS.len(), 25);
        assert_eq!(
            COUNTERS[17],
            (
                "kausa_leadership_changes_total",
                "Times this process became leader or stepped down to standby, by `to` (leader, \
                 standby)."
            )
        );
        assert_eq!(
            COUNTERS[18],
            (
                "kausa_lease_errors_total",
                "Failed calls on the Lease, by `call` (get, patch); conflicts are not errors."
            )
        );
        let names: std::collections::BTreeSet<&str> = COUNTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), COUNTERS.len(), "a counter is listed twice");
    }

    #[test]
    fn the_standby_counters_render_as_zero_when_unused() {
        let m = Metrics::default();
        let text = m.render();
        for name in ["kausa_leadership_changes_total", "kausa_lease_errors_total"] {
            assert!(
                text.contains(&format!("# TYPE {name} counter\n{name} 0\n")),
                "{name}: {text}"
            );
        }
        m.inc("kausa_leadership_changes_total", &[("to", "leader")]);
        m.inc("kausa_leadership_changes_total", &[("to", "standby")]);
        m.inc("kausa_lease_errors_total", &[("call", "get")]);
        m.add("kausa_lease_errors_total", &[("call", "patch")], 2);
        let text = respond("GET /metrics HTTP/1.1", &m).2;
        for line in [
            "kausa_leadership_changes_total{to=\"leader\"} 1\n",
            "kausa_leadership_changes_total{to=\"standby\"} 1\n",
            "kausa_lease_errors_total{call=\"get\"} 1\n",
            "kausa_lease_errors_total{call=\"patch\"} 2\n",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
    }

    #[test]
    fn the_leader_gauge_comes_after_the_counters_and_is_1_by_default() {
        // 9: the budget and queue gauges (docs/adr/0021-refill-the-call-budget.md), the
        // endpoint gauges (docs/adr/0020-check-the-endpoint.md) and the build, record and cache
        // gauges of v0.6's self-observability came later, and are not rendered until set.
        assert_eq!(GAUGES.len(), 9);
        assert_eq!(GAUGES[0], ("kausa_leader", LEADER_HELP));
        let gauge = |value: u8| {
            format!(
                "# HELP kausa_leader {LEADER_HELP}\n# TYPE kausa_leader gauge\nkausa_leader {value}\n"
            )
        };
        let m = Metrics::default();
        // Without --leader-lease, kausa always acts.
        let text = m.render();
        assert!(text.ends_with(&gauge(1)), "{text}");
        // One sample, with no label.
        let samples = text
            .lines()
            .filter(|l| l.starts_with("kausa_leader ") || l.starts_with("kausa_leader{"))
            .count();
        assert_eq!(samples, 1, "{text}");
        let last_counter = COUNTERS[COUNTERS.len() - 1].0;
        let counter_at = text
            .find(&format!("# TYPE {last_counter} counter"))
            .expect("the counter");
        let gauge_at = text.find("# TYPE kausa_leader gauge").expect("the gauge");
        assert!(counter_at < gauge_at, "{text}");
        m.set_leading(false);
        assert!(m.render().ends_with(&gauge(0)));
        m.set_leading(true);
        assert!(m.render().ends_with(&gauge(1)));
    }

    #[test]
    fn the_incident_counters_have_no_line_until_set_and_come_after_the_others() {
        // docs/adr/0028-incident-resource-and-remediation-report.md: with incidents off, the
        // exposition has none of them.
        let m = Metrics::default();
        let off = m.render();
        for (name, _) in INCIDENT_COUNTERS {
            assert!(!off.contains(name), "{name}: {off}");
            assert!(COUNTERS.iter().all(|(other, _)| *other != name), "{name}");
        }
        m.add("kausa_incidents_refused_total", &[("reason", "schema")], 0);
        m.add("kausa_incident_sweep_errors_total", &[], 0);
        let on = m.render();
        let block = "# HELP kausa_incidents_refused_total Incident objects not created, by reason \
                     (table-full, schema, starting).\n\
                     # TYPE kausa_incidents_refused_total counter\n\
                     kausa_incidents_refused_total{reason=\"schema\"} 0\n\
                     # HELP kausa_incident_sweep_errors_total Failed list or delete calls of the \
                     incident sweep.\n\
                     # TYPE kausa_incident_sweep_errors_total counter\n\
                     kausa_incident_sweep_errors_total 0\n";
        assert_eq!(on.replacen(block, "", 1), off, "{on}");
        let last_counter = COUNTERS[COUNTERS.len() - 1].0;
        let counter_at = on.find(&format!("\n{last_counter} ")).expect("the counter");
        let block_at = on.find(block).expect("the block");
        let gauge_at = on.find("# TYPE kausa_leader gauge").expect("the gauge");
        assert!(counter_at < block_at && block_at < gauge_at, "{on}");
        assert!(!on.contains("kausa_incident_write_errors_total"), "{on}");
        assert!(!on.contains("kausa_incidents_deleted_total"), "{on}");
    }

    #[test]
    fn readiness_says_whether_this_replica_acts() {
        let m = Metrics::default();
        let readyz = |m: &Metrics| {
            let (status, _, body) = respond("GET /readyz HTTP/1.1", m);
            (status, body)
        };
        assert_eq!(
            readyz(&m),
            ("503 Service Unavailable", "starting\n".to_owned())
        );
        // A standby before its first successful read of the Lease is still starting.
        m.set_leading(false);
        assert_eq!(
            readyz(&m),
            ("503 Service Unavailable", "starting\n".to_owned())
        );
        m.set_ready();
        assert_eq!(readyz(&m), ("200 OK", "standby\n".to_owned()));
        m.set_leading(true);
        assert_eq!(readyz(&m), ("200 OK", "ready\n".to_owned()));
        // Once Ready, never 503 again: neither a step-down nor a promotion takes it back.
        m.set_leading(false);
        assert_eq!(readyz(&m), ("200 OK", "standby\n".to_owned()));
        assert!(m.is_ready());
        assert_eq!(respond("GET /healthz HTTP/1.1", &m).2, "ok\n");
        // Without --leader-lease: ready once listed, as in v0.3.
        let single = Metrics::default();
        single.set_ready();
        assert_eq!(readyz(&single), ("200 OK", "ready\n".to_owned()));
    }

    // Acceptance tests for the budget and queue gauges (docs/adr/0021-refill-the-call-budget.md),
    // written before the code. They use `GAUGES: [(&str, &str); 3]` and
    // `Metrics::set_gauge(&self, name: &'static str, labels: &[(&str, &str)], value: u64)`, which
    // sets a series (labels formatted and escaped as for counters). `kausa_leader` renders as
    // before; every other gauge renders its HELP, TYPE and series only once a series was set,
    // sorted by labels.

    const BUDGET_HELP: &str = "Model calls this process may start now, by window (hour: the refill \
                               bucket within the 60-minute cap; day: the 24-hour cap).";
    const QUEUE_HELP: &str = "Incidents waiting for an explanation (at most 16).";

    #[test]
    fn gauges_render_after_kausa_leader_in_order_and_only_once_set() {
        assert_eq!(
            GAUGES[..3],
            [
                ("kausa_leader", LEADER_HELP),
                ("kausa_budget_calls_remaining", BUDGET_HELP),
                ("kausa_explanation_queue_length", QUEUE_HELP),
            ]
        );
        let leader = |value: u8| {
            format!(
                "# HELP kausa_leader {LEADER_HELP}\n# TYPE kausa_leader gauge\nkausa_leader {value}\n"
            )
        };
        let budget = |hour: u64, day: u64| {
            format!(
                "# HELP kausa_budget_calls_remaining {BUDGET_HELP}\n\
                 # TYPE kausa_budget_calls_remaining gauge\n\
                 kausa_budget_calls_remaining{{window=\"day\"}} {day}\n\
                 kausa_budget_calls_remaining{{window=\"hour\"}} {hour}\n"
            )
        };
        let queue = |length: u64| {
            format!(
                "# HELP kausa_explanation_queue_length {QUEUE_HELP}\n\
                 # TYPE kausa_explanation_queue_length gauge\n\
                 kausa_explanation_queue_length {length}\n"
            )
        };
        // A gauge never set is not rendered: no 0 that an alert could fire on.
        let m = Metrics::default();
        let text = m.render();
        assert!(text.ends_with(&leader(1)), "{text}");
        assert!(!text.contains("kausa_budget_calls_remaining"), "{text}");
        assert!(!text.contains("kausa_explanation_queue_length"), "{text}");
        // The queue alone (a dry run).
        m.set_gauge("kausa_explanation_queue_length", &[], 0);
        let text = m.render();
        assert!(
            text.ends_with(&format!("{}{}", leader(1), queue(0))),
            "{text}"
        );
        assert!(!text.contains("kausa_budget_calls_remaining"), "{text}");
        // Both, in the order of GAUGES, the day series before the hour series.
        m.set_gauge("kausa_budget_calls_remaining", &[("window", "hour")], 3);
        m.set_gauge("kausa_budget_calls_remaining", &[("window", "day")], 200);
        let text = m.render();
        assert!(
            text.ends_with(&format!("{}{}{}", leader(1), budget(3, 200), queue(0))),
            "{text}"
        );
        // A gauge is set, not added to.
        m.set_gauge("kausa_budget_calls_remaining", &[("window", "hour")], 1);
        m.set_gauge("kausa_explanation_queue_length", &[], 16);
        m.set_gauge("kausa_explanation_queue_length", &[], 15);
        m.set_leading(false);
        let text = m.render();
        assert!(
            text.ends_with(&format!("{}{}{}", leader(0), budget(1, 200), queue(15))),
            "{text}"
        );
        // One sample per series.
        for series in [
            "kausa_budget_calls_remaining{window=\"hour\"} ",
            "kausa_budget_calls_remaining{window=\"day\"} ",
            "kausa_explanation_queue_length ",
        ] {
            assert_eq!(
                text.lines().filter(|l| l.starts_with(series)).count(),
                1,
                "{series}: {text}"
            );
        }
    }

    #[test]
    fn gauge_label_values_are_escaped() {
        let m = Metrics::default();
        m.set_gauge(
            "kausa_budget_calls_remaining",
            &[("window", "a\"b\\c\nd")],
            2,
        );
        assert!(
            m.render()
                .contains(r#"kausa_budget_calls_remaining{window="a\"b\\c\nd"} 2"#)
        );
    }

    // Acceptance tests for docs/adr/0022-redact-the-answer.md, written before the code: one
    // counter appended after the standby's, by rule (an id of `redact::ANSWER_RULES`), and the
    // help of `kausa_redactions_total` saying that it counts the requests. Found by name, so that
    // a counter appended later does not break them.

    const ANSWER_REDACTIONS_HELP: &str =
        "Values replaced by redaction in the model's answers, by rule.";

    fn counter_at(name: &str) -> usize {
        COUNTERS
            .iter()
            .position(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("no counter {name}"))
    }

    #[test]
    fn the_answer_redactions_counter_comes_after_the_standby_counters() {
        let at = counter_at("kausa_answer_redactions_total");
        assert_eq!(at, counter_at("kausa_lease_errors_total") + 1);
        assert_eq!(COUNTERS[at].1, ANSWER_REDACTIONS_HELP);
        assert_eq!(
            COUNTERS[counter_at("kausa_redactions_total")].1,
            "Values replaced by redaction in the requests, by rule."
        );
        let names: std::collections::BTreeSet<&str> = COUNTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), COUNTERS.len(), "a counter is listed twice");
    }

    #[test]
    fn the_answer_redactions_counter_renders_as_zero_when_unused() {
        let m = Metrics::default();
        let text = m.render();
        assert!(
            text.contains(&format!(
                "# HELP kausa_answer_redactions_total {ANSWER_REDACTIONS_HELP}\n\
                 # TYPE kausa_answer_redactions_total counter\n\
                 kausa_answer_redactions_total 0\n"
            )),
            "{text}"
        );
        m.add("kausa_answer_redactions_total", &[("rule", "R004")], 3);
        m.inc("kausa_answer_redactions_total", &[("rule", "R013")]);
        m.inc("kausa_redactions_total", &[("rule", "R004")]);
        let text = respond("GET /metrics HTTP/1.1", &m).2;
        for line in [
            "kausa_answer_redactions_total{rule=\"R004\"} 3\n",
            "kausa_answer_redactions_total{rule=\"R013\"} 1\n",
            "kausa_redactions_total{rule=\"R004\"} 1\n",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
        assert!(
            !text.contains("kausa_answer_redactions_total 0\n"),
            "{text}"
        );
    }

    // Acceptance tests for the endpoint gauges (docs/adr/0020-check-the-endpoint.md), written
    // before the code. They use `GAUGES: [(&str, &str); 5]`, the two new gauges appended after
    // the queue gauge, and `set_gauge` as the 0021 gauges do: `kausa_endpoint_state` has three
    // series, by `state`, exactly one of them 1; `kausa_model_last_success_timestamp_seconds` has
    // one, 0 before the first chat completion. No counter is added.

    const ENDPOINT_HELP: &str = "State of the model endpoint's breaker: 1 for the current state \
                                 (closed, open, half_open), 0 for the others; closed on a standby.";
    const LAST_SUCCESS_HELP: &str =
        "Unix time of the last call answered with a chat completion, 0 before the first.";

    /// Sets the endpoint gauge as kausa does: `state` at 1, the other two at 0.
    fn show_state(m: &Metrics, state: &str) {
        for s in ["closed", "open", "half_open"] {
            m.set_gauge(
                "kausa_endpoint_state",
                &[("state", s)],
                u64::from(s == state),
            );
        }
    }

    #[test]
    fn the_endpoint_gauges_follow_the_queue_gauge_with_their_help() {
        // By position: the build, record and cache gauges of v0.6's self-observability follow
        // these, and the watch counters came with them; the withheld answers of
        // docs/adr/0025-withhold-destructive-commands.md came later (23 counters).
        assert_eq!(COUNTERS.len(), 25, "a counter was added");
        assert_eq!(
            GAUGES[..5],
            [
                ("kausa_leader", LEADER_HELP),
                ("kausa_budget_calls_remaining", BUDGET_HELP),
                ("kausa_explanation_queue_length", QUEUE_HELP),
                ("kausa_endpoint_state", ENDPOINT_HELP),
                (
                    "kausa_model_last_success_timestamp_seconds",
                    LAST_SUCCESS_HELP
                ),
            ]
        );
        let names: std::collections::BTreeSet<&str> = GAUGES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), GAUGES.len(), "a gauge is listed twice");
    }

    #[test]
    fn the_endpoint_state_renders_three_series_under_one_help_with_one_at_1() {
        let state = |closed: u8, half_open: u8, open: u8| {
            format!(
                "# HELP kausa_endpoint_state {ENDPOINT_HELP}\n\
                 # TYPE kausa_endpoint_state gauge\n\
                 kausa_endpoint_state{{state=\"closed\"}} {closed}\n\
                 kausa_endpoint_state{{state=\"half_open\"}} {half_open}\n\
                 kausa_endpoint_state{{state=\"open\"}} {open}\n"
            )
        };
        let last = |seconds: u64| {
            format!(
                "# HELP kausa_model_last_success_timestamp_seconds {LAST_SUCCESS_HELP}\n\
                 # TYPE kausa_model_last_success_timestamp_seconds gauge\n\
                 kausa_model_last_success_timestamp_seconds {seconds}\n"
            )
        };
        let m = Metrics::default();
        // Never set, never rendered.
        let text = m.render();
        assert!(!text.contains("kausa_endpoint_state"), "{text}");
        assert!(
            !text.contains("kausa_model_last_success_timestamp_seconds"),
            "{text}"
        );
        // Without --endpoint, the state alone, closed; after the queue gauge.
        m.set_gauge("kausa_explanation_queue_length", &[], 0);
        show_state(&m, "closed");
        let text = m.render();
        assert!(text.ends_with(&state(1, 0, 0)), "{text}");
        assert!(
            text.find("# TYPE kausa_explanation_queue_length")
                < text.find("# TYPE kausa_endpoint_state"),
            "{text}"
        );
        // With --endpoint, the last success too: 0 before the first chat completion.
        m.set_gauge("kausa_model_last_success_timestamp_seconds", &[], 0);
        let text = m.render();
        assert!(
            text.ends_with(&format!("{}{}", state(1, 0, 0), last(0))),
            "{text}"
        );
        for (current, expected) in [
            ("open", state(0, 0, 1)),
            ("half_open", state(0, 1, 0)),
            ("closed", state(1, 0, 0)),
        ] {
            show_state(&m, current);
            m.set_gauge(
                "kausa_model_last_success_timestamp_seconds",
                &[],
                1_793_620_800,
            );
            let text = m.render();
            assert!(
                text.ends_with(&format!("{expected}{}", last(1_793_620_800))),
                "{current}: {text}"
            );
            assert_eq!(
                text.matches("# HELP kausa_endpoint_state ").count(),
                1,
                "{text}"
            );
            let ones = text
                .lines()
                .filter(|l| l.starts_with("kausa_endpoint_state{") && l.ends_with(" 1"))
                .count();
            assert_eq!(ones, 1, "{current}: {text}");
        }
    }

    // Acceptance tests for v0.6's self-observability (no ADR), written before the code. They use:
    // - `COUNTERS: [(&str, &str); 22]`: `kausa_watch_relists_total`, then
    //   `kausa_watch_restarts_total`, appended after `kausa_answer_redactions_total`;
    // - `GAUGES: [(&str, &str); 9]`: `kausa_build_info`, `kausa_records`,
    //   `kausa_owner_cache_entries` and `kausa_dedup_cache_entries`, in that order, appended after
    //   `kausa_model_last_success_timestamp_seconds`, rendered only once set;
    // - `pub const CALL_DURATION_BUCKETS: [f64; 11]` and
    //   `pub const HISTOGRAMS: [(&str, &str, &[f64]); 1]`;
    // - `Metrics::observe(&self, name: &'static str, labels: &[(&str, &str)], value: f64)`: the
    //   first bucket whose bound is at least `value` counts it (none: only `+Inf`), `_sum` adds it
    //   and `_count` adds 1. A histogram renders after the gauges, only once observed: `# HELP`,
    //   `# TYPE <name> histogram`, then per series (sorted by labels) one cumulative
    //   `_bucket{<labels>,le="<bound>"}` line per bound in bucket order, `le="+Inf"` with the
    //   count, `_sum` and `_count`; bounds and sums written with Rust's `{}` for `f64`.

    const RELISTS_HELP: &str =
        "Lists of the Pod, Event and Deployment watches after the first of each watch, by watch.";
    const RESTARTS_HELP: &str =
        "New watchers started after the API server refused a watch (403), by watch.";
    const BUILD_INFO_HELP: &str =
        "Always 1; the labels name this build of kausa (version, prompt_version).";
    const RECORDS_HELP: &str =
        "Records kausa holds, by kind (pods, events, deployments); 0 on a standby.";
    const OWNER_CACHE_HELP: &str = "Owner lookups in the cache, expired ones included until the \
                                    next lookup that reads the API server; 0 on a standby.";
    const DEDUP_CACHE_HELP: &str = "Explanations in the deduplication cache (at most 1024), \
                                    expired ones included until read again or evicted.";
    const CALL_DURATION: &str = "kausa_model_call_duration_seconds";
    const CALL_DURATION_HELP: &str =
        "Duration of the calls to the model endpoint, by outcome (ok, error, invalid).";

    #[test]
    fn the_watch_relist_and_restart_counters_come_last_with_their_help() {
        // The withheld answers of docs/adr/0025-withhold-destructive-commands.md come after
        // these, and the two counters of docs/adr/0027-fact-statement-for-the-detections.md.
        assert_eq!(COUNTERS.len(), 25);
        assert_eq!(
            COUNTERS[20..22],
            [
                ("kausa_watch_relists_total", RELISTS_HELP),
                ("kausa_watch_restarts_total", RESTARTS_HELP),
            ]
        );
        assert_eq!(
            counter_at("kausa_watch_relists_total"),
            counter_at("kausa_answer_redactions_total") + 1
        );
        let names: std::collections::BTreeSet<&str> = COUNTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), COUNTERS.len(), "a counter is listed twice");
        for (name, _) in COUNTERS {
            assert!(name.ends_with("_total"), "{name}");
        }
    }

    #[test]
    fn the_watch_relist_and_restart_counters_render_as_zero_when_unused() {
        let m = Metrics::default();
        let text = m.render();
        for (name, help) in [
            ("kausa_watch_relists_total", RELISTS_HELP),
            ("kausa_watch_restarts_total", RESTARTS_HELP),
        ] {
            assert!(
                text.contains(&format!(
                    "# HELP {name} {help}\n# TYPE {name} counter\n{name} 0\n"
                )),
                "{name}: {text}"
            );
        }
        m.inc("kausa_watch_relists_total", &[("watch", "pods")]);
        m.add("kausa_watch_restarts_total", &[("watch", "events")], 2);
        let text = respond("GET /metrics HTTP/1.1", &m).2;
        for line in [
            "kausa_watch_relists_total{watch=\"pods\"} 1\n",
            "kausa_watch_restarts_total{watch=\"events\"} 2\n",
        ] {
            assert!(text.contains(line), "{line}: {text}");
        }
        assert!(!text.contains("kausa_watch_relists_total 0\n"), "{text}");
    }

    #[test]
    fn the_build_record_and_cache_gauges_follow_the_last_success_with_their_help() {
        assert_eq!(GAUGES.len(), 9);
        assert_eq!(GAUGES[4].0, "kausa_model_last_success_timestamp_seconds");
        assert_eq!(
            GAUGES[5..],
            [
                ("kausa_build_info", BUILD_INFO_HELP),
                ("kausa_records", RECORDS_HELP),
                ("kausa_owner_cache_entries", OWNER_CACHE_HELP),
                ("kausa_dedup_cache_entries", DEDUP_CACHE_HELP),
            ]
        );
        let names: std::collections::BTreeSet<&str> = GAUGES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), GAUGES.len(), "a gauge is listed twice");
        // Never set, never rendered: no 0 an alert could fire on.
        let m = Metrics::default();
        let text = m.render();
        for (name, _) in &GAUGES[5..] {
            assert!(!text.contains(name), "{name}: {text}");
        }
        // Once set: in the order of GAUGES, after the last success, the series sorted by labels.
        m.set_gauge("kausa_model_last_success_timestamp_seconds", &[], 0);
        m.set_gauge("kausa_dedup_cache_entries", &[], 2);
        m.set_gauge("kausa_owner_cache_entries", &[], 3);
        m.set_gauge("kausa_records", &[("kind", "pods")], 7);
        m.set_gauge("kausa_records", &[("kind", "events")], 4);
        m.set_gauge("kausa_records", &[("kind", "deployments")], 1);
        m.set_gauge(
            "kausa_build_info",
            &[("version", "0.0.0-test"), ("prompt_version", "2")],
            1,
        );
        let expected = format!(
            "# TYPE kausa_model_last_success_timestamp_seconds gauge\n\
             kausa_model_last_success_timestamp_seconds 0\n\
             # HELP kausa_build_info {BUILD_INFO_HELP}\n\
             # TYPE kausa_build_info gauge\n\
             kausa_build_info{{version=\"0.0.0-test\",prompt_version=\"2\"}} 1\n\
             # HELP kausa_records {RECORDS_HELP}\n\
             # TYPE kausa_records gauge\n\
             kausa_records{{kind=\"deployments\"}} 1\n\
             kausa_records{{kind=\"events\"}} 4\n\
             kausa_records{{kind=\"pods\"}} 7\n\
             # HELP kausa_owner_cache_entries {OWNER_CACHE_HELP}\n\
             # TYPE kausa_owner_cache_entries gauge\n\
             kausa_owner_cache_entries 3\n\
             # HELP kausa_dedup_cache_entries {DEDUP_CACHE_HELP}\n\
             # TYPE kausa_dedup_cache_entries gauge\n\
             kausa_dedup_cache_entries 2\n"
        );
        let text = m.render();
        assert!(text.ends_with(&expected), "{text}");
        // A gauge is set, not added to.
        m.set_gauge("kausa_records", &[("kind", "pods")], 0);
        assert!(
            m.render().contains("kausa_records{kind=\"pods\"} 0\n"),
            "{text}"
        );
    }

    #[test]
    fn build_info_names_this_build_in_one_sample_at_1() {
        assert_eq!(crate::prompt::PROMPT_VERSION, "3");
        let m = Metrics::default();
        // As `kausa run` sets it at start.
        m.set_gauge(
            "kausa_build_info",
            &[
                ("version", env!("CARGO_PKG_VERSION")),
                ("prompt_version", crate::prompt::PROMPT_VERSION),
            ],
            1,
        );
        let text = m.render();
        let line = format!(
            "kausa_build_info{{version=\"{}\",prompt_version=\"3\"}} 1\n",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            text.contains(&format!(
                "# HELP kausa_build_info {BUILD_INFO_HELP}\n# TYPE kausa_build_info gauge\n{line}"
            )),
            "{text}"
        );
        let samples = text
            .lines()
            .filter(|l| l.starts_with("kausa_build_info"))
            .count();
        assert_eq!(samples, 1, "{text}");
    }

    /// The lines of one series of the call-duration histogram: `labels` is the label set before
    /// `le` (empty for none), `cumulative` the count at each bound of `CALL_DURATION_BUCKETS`.
    fn histogram_series(labels: &str, cumulative: [u64; 11], count: u64, sum: &str) -> String {
        let bounds = [
            "0.5", "1", "2.5", "5", "10", "20", "30", "60", "120", "300", "600",
        ];
        let (open, sep, close) = if labels.is_empty() {
            ("", "", "")
        } else {
            ("{", ",", "}")
        };
        let mut out = String::new();
        for (bound, n) in bounds.iter().zip(cumulative) {
            out.push_str(&format!(
                "{CALL_DURATION}_bucket{{{labels}{sep}le=\"{bound}\"}} {n}\n"
            ));
        }
        out.push_str(&format!(
            "{CALL_DURATION}_bucket{{{labels}{sep}le=\"+Inf\"}} {count}\n"
        ));
        out.push_str(&format!("{CALL_DURATION}_sum{open}{labels}{close} {sum}\n"));
        out.push_str(&format!(
            "{CALL_DURATION}_count{open}{labels}{close} {count}\n"
        ));
        out
    }

    #[test]
    fn the_call_duration_histogram_has_its_buckets_and_help() {
        assert_eq!(
            CALL_DURATION_BUCKETS,
            [
                0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0
            ]
        );
        assert_eq!(HISTOGRAMS.len(), 1);
        let (name, help, bounds) = HISTOGRAMS[0];
        assert_eq!((name, help), (CALL_DURATION, CALL_DURATION_HELP));
        assert_eq!(bounds, &CALL_DURATION_BUCKETS[..]);
        assert!(bounds.windows(2).all(|w| w[0] < w[1]), "bounds in order");
        // No name is both a histogram and a counter or a gauge, nor ends like a histogram's series.
        for (other, _) in COUNTERS.iter().chain(GAUGES.iter()) {
            assert_ne!(*other, name);
            for suffix in ["_bucket", "_sum", "_count"] {
                assert!(!other.ends_with(suffix), "{other}");
            }
        }
    }

    #[test]
    fn the_call_duration_histogram_renders_cumulative_buckets_after_the_gauges() {
        let m = Metrics::default();
        // Never observed: no line at all, not even its help.
        m.set_gauge("kausa_dedup_cache_entries", &[], 0);
        let text = m.render();
        assert!(!text.contains(CALL_DURATION), "{text}");
        // 1.0 lies on a bound and counts in le="1"; 700.0 is past the last bound: +Inf only.
        m.observe(CALL_DURATION, &[("outcome", "ok")], 0.25);
        m.observe(CALL_DURATION, &[("outcome", "ok")], 1.0);
        m.observe(CALL_DURATION, &[("outcome", "ok")], 700.0);
        m.observe(CALL_DURATION, &[("outcome", "error")], 3.0);
        let expected = format!(
            "# HELP {CALL_DURATION} {CALL_DURATION_HELP}\n# TYPE {CALL_DURATION} histogram\n{}{}",
            histogram_series(
                "outcome=\"error\"",
                [0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
                1,
                "3"
            ),
            histogram_series(
                "outcome=\"ok\"",
                [1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2],
                3,
                "701.25"
            ),
        );
        let text = m.render();
        assert!(text.ends_with(&expected), "{text}");
        assert_eq!(
            text.matches(&format!("# TYPE {CALL_DURATION} histogram\n"))
                .count(),
            1,
            "{text}"
        );
        assert!(
            text.find("# TYPE kausa_dedup_cache_entries gauge")
                < text.find(&format!("# TYPE {CALL_DURATION} histogram")),
            "{text}"
        );
        assert!(
            text.find("# TYPE kausa_leader gauge")
                < text.find(&format!("# TYPE {CALL_DURATION} histogram")),
            "{text}"
        );
        // The same value again adds to the same series.
        m.observe(CALL_DURATION, &[("outcome", "error")], 3.0);
        assert!(
            m.render().contains(&histogram_series(
                "outcome=\"error\"",
                [0, 0, 0, 2, 2, 2, 2, 2, 2, 2, 2],
                2,
                "6"
            )),
            "{text}"
        );
    }

    #[test]
    fn a_histogram_without_labels_renders_le_alone_and_bare_sum_and_count() {
        let m = Metrics::default();
        m.observe(CALL_DURATION, &[], 0.5);
        let text = m.render();
        assert!(
            text.ends_with(&histogram_series(
                "",
                [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
                1,
                "0.5"
            )),
            "{text}"
        );
    }

    #[test]
    fn histogram_label_values_are_escaped() {
        let m = Metrics::default();
        m.observe(CALL_DURATION, &[("outcome", "a\"b\\c\nd")], 0.1);
        let text = m.render();
        for line in [
            r#"kausa_model_call_duration_seconds_bucket{outcome="a\"b\\c\nd",le="0.5"} 1"#,
            r#"kausa_model_call_duration_seconds_bucket{outcome="a\"b\\c\nd",le="+Inf"} 1"#,
            r#"kausa_model_call_duration_seconds_sum{outcome="a\"b\\c\nd"} 0.1"#,
            r#"kausa_model_call_duration_seconds_count{outcome="a\"b\\c\nd"} 1"#,
        ] {
            assert!(text.contains(&format!("{line}\n")), "{line}: {text}");
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic]
    fn an_unknown_histogram_is_a_bug() {
        Metrics::default().observe("kausa_no_such_histogram_seconds", &[], 1.0);
    }

    /// Every counter, gauge and histogram series kausa can show, with fixed values: the fixture of
    /// tests/golden/metrics.prom. The build info names a fixed version, so that a release, which
    /// moves `CARGO_PKG_VERSION`, does not change the file.
    fn golden_fixture() -> Metrics {
        let m = Metrics::default();
        for (name, labels, by) in [
            ("kausa_incidents_total", vec![("detection", "D001")], 4),
            ("kausa_incidents_total", vec![("detection", "D014")], 1),
            ("kausa_model_calls_total", vec![("outcome", "ok")], 5),
            ("kausa_model_calls_total", vec![("outcome", "error")], 2),
            ("kausa_model_calls_total", vec![("outcome", "invalid")], 1),
            ("kausa_model_tokens_total", vec![("direction", "in")], 1200),
            ("kausa_model_tokens_total", vec![("direction", "out")], 300),
            ("kausa_explanations_cached_total", vec![], 3),
            (
                "kausa_explanations_skipped_total",
                vec![("reason", "budget")],
                1,
            ),
            (
                "kausa_explanations_skipped_total",
                vec![("reason", "context_too_large")],
                0,
            ),
            (
                "kausa_explanations_skipped_total",
                vec![("reason", "endpoint")],
                2,
            ),
            (
                "kausa_explanations_skipped_total",
                vec![("reason", "invalid_answer")],
                1,
            ),
            (
                "kausa_explanations_skipped_total",
                vec![("reason", "endpoint_unavailable")],
                0,
            ),
            ("kausa_explanations_retried_total", vec![], 1),
            ("kausa_explanations_dropped_total", vec![], 0),
            ("kausa_requests_logged_total", vec![], 0),
            ("kausa_redactions_total", vec![("rule", "R004")], 7),
            ("kausa_redactions_total", vec![("rule", "R010")], 2),
            (
                "kausa_events_written_total",
                vec![("reason", "Explained")],
                5,
            ),
            (
                "kausa_events_written_total",
                vec![("reason", "ExplanationSkipped")],
                4,
            ),
            (
                "kausa_events_written_total",
                vec![("reason", "Detected")],
                6,
            ),
            ("kausa_event_write_errors_total", vec![], 1),
            ("kausa_watch_errors_total", vec![("watch", "pods")], 2),
            ("kausa_watch_errors_total", vec![("watch", "events")], 3),
            (
                "kausa_watch_errors_total",
                vec![("watch", "deployments")],
                0,
            ),
            (
                "kausa_history_writes_total",
                vec![("outcome", "created")],
                4,
            ),
            (
                "kausa_history_writes_total",
                vec![("outcome", "updated")],
                2,
            ),
            (
                "kausa_history_writes_total",
                vec![("outcome", "refused")],
                0,
            ),
            (
                "kausa_history_writes_total",
                vec![("outcome", "conflict")],
                1,
            ),
            ("kausa_history_writes_total", vec![("outcome", "error")], 0),
            ("kausa_history_deleted_total", vec![("reason", "age")], 2),
            ("kausa_history_deleted_total", vec![("reason", "cap")], 1),
            ("kausa_history_sweep_errors_total", vec![], 0),
            ("kausa_notifications_total", vec![("outcome", "sent")], 4),
            ("kausa_notifications_total", vec![("outcome", "failed")], 1),
            ("kausa_notifications_total", vec![("outcome", "dropped")], 0),
            ("kausa_notifications_total", vec![("outcome", "capped")], 3),
            ("kausa_notifications_retried_total", vec![], 1),
            ("kausa_leadership_changes_total", vec![("to", "leader")], 2),
            ("kausa_leadership_changes_total", vec![("to", "standby")], 1),
            ("kausa_lease_errors_total", vec![("call", "get")], 0),
            ("kausa_lease_errors_total", vec![("call", "patch")], 1),
            ("kausa_answer_redactions_total", vec![("rule", "R004")], 1),
            ("kausa_watch_relists_total", vec![("watch", "pods")], 2),
            ("kausa_watch_relists_total", vec![("watch", "events")], 1),
            (
                "kausa_watch_relists_total",
                vec![("watch", "deployments")],
                0,
            ),
            ("kausa_watch_restarts_total", vec![("watch", "pods")], 1),
            ("kausa_watch_restarts_total", vec![("watch", "events")], 0),
            (
                "kausa_watch_restarts_total",
                vec![("watch", "deployments")],
                0,
            ),
            (
                "kausa_incidents_refused_total",
                vec![("reason", "table-full")],
                1,
            ),
            (
                "kausa_incidents_refused_total",
                vec![("reason", "schema")],
                0,
            ),
            (
                "kausa_incidents_refused_total",
                vec![("reason", "starting")],
                2,
            ),
            ("kausa_incident_write_errors_total", vec![], 1),
            ("kausa_incidents_deleted_total", vec![("reason", "age")], 3),
            ("kausa_incidents_deleted_total", vec![("reason", "cap")], 0),
            ("kausa_incident_sweep_errors_total", vec![], 0),
        ] {
            m.add(name, &labels, by);
        }
        for (name, labels, value) in [
            ("kausa_budget_calls_remaining", vec![("window", "hour")], 3),
            ("kausa_budget_calls_remaining", vec![("window", "day")], 195),
            ("kausa_explanation_queue_length", vec![], 2),
            ("kausa_endpoint_state", vec![("state", "closed")], 1),
            ("kausa_endpoint_state", vec![("state", "half_open")], 0),
            ("kausa_endpoint_state", vec![("state", "open")], 0),
            (
                "kausa_model_last_success_timestamp_seconds",
                vec![],
                1_793_620_800,
            ),
            (
                "kausa_build_info",
                vec![("version", "0.0.0-golden"), ("prompt_version", "2")],
                1,
            ),
            ("kausa_records", vec![("kind", "pods")], 9),
            ("kausa_records", vec![("kind", "events")], 12),
            ("kausa_records", vec![("kind", "deployments")], 1),
            ("kausa_owner_cache_entries", vec![], 4),
            ("kausa_dedup_cache_entries", vec![], 5),
        ] {
            m.set_gauge(name, &labels, value);
        }
        m.set_leading(true);
        for (outcome, seconds) in [
            ("ok", 0.5),
            ("ok", 0.75),
            ("ok", 2.0),
            ("ok", 4.5),
            ("ok", 130.0),
            ("error", 10.0),
            ("error", 0.25),
            ("invalid", 1.5),
        ] {
            m.observe(CALL_DURATION, &[("outcome", outcome)], seconds);
        }
        m
    }

    #[test]
    fn the_exposition_is_the_golden_file() {
        // promtool check metrics lints the same file in the ci target. On a change of a metric,
        // edit the file by hand, as the check-endpoint goldens are.
        let golden = include_str!("../tests/golden/metrics.prom");
        let text = golden_fixture().render();
        if text != golden {
            panic!(
                "render() differs from tests/golden/metrics.prom; render() gave:\n{text}\n\
                 first differing line: {:?}",
                text.lines()
                    .zip(golden.lines())
                    .position(|(a, b)| a != b)
                    .map(|i| i + 1)
            );
        }
        // Every counter, gauge and histogram of the lists has a series in it.
        for (name, _) in COUNTERS
            .iter()
            .chain(INCIDENT_COUNTERS.iter())
            .chain(GAUGES.iter())
        {
            assert!(
                golden.contains(&format!("\n{name}{{")) || golden.contains(&format!("\n{name} ")),
                "{name} has no series in the golden file"
            );
        }
        for (name, _, _) in HISTOGRAMS {
            assert!(
                golden.contains(&format!("# TYPE {name} histogram\n")),
                "{name}"
            );
        }
        // A fixed version, so that a release does not change the file.
        assert!(
            golden.contains("kausa_build_info{version=\"0.0.0-golden\",prompt_version=\"2\"} 1\n")
        );
    }

    /// One alert of deploy/prometheus/kausa-alerts.yaml, read line by line: the file is plain
    /// YAML in one fixed layout, which promtool checks in the ci target.
    #[derive(Debug, Default)]
    struct Alert {
        name: String,
        expr: String,
        labels: Vec<String>,
        runbook_url: String,
    }

    fn indent(line: &str) -> usize {
        line.len() - line.trim_start().len()
    }

    fn alerts(rules: &str) -> Vec<Alert> {
        let lines: Vec<&str> = rules.lines().collect();
        let mut alerts: Vec<Alert> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim_start();
            if let Some(name) = trimmed.strip_prefix("- alert: ") {
                alerts.push(Alert {
                    name: name.to_string(),
                    ..Alert::default()
                });
            } else if let Some(alert) = alerts.last_mut() {
                // The lines nested under this one: a block expression or the labels.
                let nested = || {
                    lines[i + 1..]
                        .iter()
                        .take_while(|l| l.trim().is_empty() || indent(l) > indent(line))
                        .copied()
                        .collect::<Vec<_>>()
                };
                if trimmed == "expr: |" {
                    let block = nested();
                    let cut = block
                        .iter()
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| indent(l))
                        .min()
                        .unwrap_or(0);
                    alert.expr = block
                        .iter()
                        .map(|l| l.get(cut..).unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim_end()
                        .to_string();
                    i += block.len();
                } else if let Some(expr) = trimmed.strip_prefix("expr: ") {
                    alert.expr = expr.to_string();
                } else if trimmed == "labels:" {
                    let block = nested();
                    alert.labels = block
                        .iter()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect();
                    i += block.len();
                } else if let Some(url) = trimmed.strip_prefix("runbook_url: ") {
                    alert.runbook_url = url.to_string();
                }
            }
            i += 1;
        }
        alerts
    }

    #[test]
    fn the_alert_rules_read_only_kausas_metrics_and_each_has_its_section_in_alerts_md() {
        let rules = include_str!("../deploy/prometheus/kausa-alerts.yaml");
        let reference = include_str!("../docs/reference/alerts.md");
        let alerts = alerts(rules);
        assert_eq!(alerts.len(), 12, "the alerts of the rules file");

        // Every metric a rule (or a comment) names is one kausa exports: a histogram also by its
        // _bucket, _sum and _count series.
        let mut known: Vec<String> = COUNTERS
            .iter()
            .chain(GAUGES.iter())
            .map(|(name, _)| name.to_string())
            .collect();
        for (name, _, _) in HISTOGRAMS {
            for suffix in ["", "_bucket", "_sum", "_count"] {
                known.push(format!("{name}{suffix}"));
            }
        }
        let token = regex::Regex::new("kausa_[a-z_]+").unwrap();
        let read: Vec<&str> = token.find_iter(rules).map(|m| m.as_str()).collect();
        assert!(!read.is_empty());
        for name in &read {
            assert!(
                known.iter().any(|k| k == name),
                "{name} is in the rules file but kausa exports no such metric"
            );
        }

        let headings: Vec<&str> = reference
            .lines()
            .filter_map(|l| l.strip_prefix("### "))
            .collect();
        for alert in &alerts {
            let name = &alert.name;
            assert_eq!(
                alert.labels,
                ["severity: warning"],
                "{name}: severity: warning is the only label"
            );
            assert_eq!(
                alert.runbook_url,
                format!(
                    "https://github.com/fedeam/kausa/blob/main/docs/reference/alerts.md#{}",
                    name.to_lowercase()
                ),
                "{name}: runbook_url"
            );
            assert!(
                headings.contains(&name.as_str()),
                "{name} has no ### {name} section in docs/reference/alerts.md"
            );
            // The section's expression is the rule's, as the file holds it.
            let section = reference
                .split_once(&format!("\n### {name}\n"))
                .map(|(_, rest)| rest)
                .unwrap_or_default();
            let shown = section
                .split_once("```promql\n")
                .and_then(|(_, rest)| rest.split_once("\n```"))
                .map(|(expr, _)| expr)
                .unwrap_or_default();
            assert!(!alert.expr.is_empty(), "{name} has no expr");
            assert_eq!(
                shown, alert.expr,
                "{name}: the promql block of its section differs from the rules file"
            );
        }
        for heading in headings.iter().filter(|h| h.starts_with("Kausa")) {
            assert!(
                alerts.iter().any(|a| a.name == *heading),
                "### {heading} in docs/reference/alerts.md is no alert of the rules file"
            );
        }
    }

    // Acceptance tests for docs/adr/0025-withhold-destructive-commands.md, written before the
    // code: one counter with no labels, appended last to `COUNTERS`, so it renders 0 from the
    // start and needs no zero series in run.rs. No alert reads it yet.

    const WITHHELD_HELP: &str =
        "Model answers in which kausa replaced text with its withheld-command notice.";

    #[test]
    fn the_withheld_counter_comes_last_with_its_help() {
        assert_eq!(COUNTERS.len(), 25);
        assert_eq!(
            COUNTERS[22],
            ("kausa_answers_withheld_total", WITHHELD_HELP)
        );
        assert_eq!(
            counter_at("kausa_answers_withheld_total"),
            counter_at("kausa_watch_restarts_total") + 1
        );
        let names: std::collections::BTreeSet<&str> = COUNTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), COUNTERS.len(), "a counter is listed twice");
    }

    #[test]
    fn the_withheld_counter_renders_as_zero_when_unused() {
        let m = Metrics::default();
        let name = "kausa_answers_withheld_total";
        assert!(
            m.render().contains(&format!(
                "# HELP {name} {WITHHELD_HELP}\n# TYPE {name} counter\n{name} 0\n"
            )),
            "no zero series"
        );
        m.inc(name, &[]);
        let text = respond("GET /metrics HTTP/1.1", &m).2;
        assert!(text.contains(&format!("\n{name} 1\n")), "the count");
        assert!(
            !text.contains(&format!("\n{name} 0\n")),
            "a zero series is left"
        );
    }

    // --- The connection: `answer` reads the request line to its end before it answers. A proxy
    // between the client and kausa may pass on a request in pieces, the first byte on its own.

    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::time::Instant;

    /// Answers the one connection of `client`, which runs in a thread, and returns what the
    /// client read until kausa closed the connection.
    fn answered(client: impl FnOnce(&mut std::net::TcpStream) + Send + 'static) -> String {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("a port");
            let address = listener.local_addr().expect("an address");
            let thread = std::thread::spawn(move || {
                let mut stream = std::net::TcpStream::connect(address).expect("a connection");
                // Each write leaves at once, as its own piece.
                stream.set_nodelay(true).expect("no delay");
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .expect("a read timeout");
                client(&mut stream);
                let mut answer = Vec::new();
                let _ = stream.read_to_end(&mut answer);
                String::from_utf8(answer).expect("UTF-8")
            });
            let (stream, _) = listener.accept().await.expect("a connection");
            answer(stream, &Metrics::default()).await;
            thread.join().expect("the client")
        })
    }

    /// Writes each piece on its own, with a pause long enough for kausa to read it first.
    fn pieces(stream: &mut std::net::TcpStream, pieces: &[&str]) {
        for piece in pieces {
            stream.write_all(piece.as_bytes()).expect("a piece");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn a_request_whose_first_byte_arrives_alone_is_answered_whole() {
        let answer = answered(|stream| pieces(stream, &["G", "ET /metrics HTTP/1.1\r\n\r\n"]));
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(answer.contains("\nkausa_leader 1\n"), "{answer}");
    }

    #[test]
    fn a_request_line_in_pieces_is_answered_once_it_ends() {
        let answer = answered(|stream| pieces(stream, &["GET /hea", "lthz HTT", "P/1.1\r", "\n"]));
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(answer.ends_with("\r\n\r\nok\n"), "{answer}");
        // The method still decides: one that only begins like `GET` is not `GET`.
        let answer = answered(|stream| pieces(stream, &["G", " /metrics HTTP/1.1\r\n\r\n"]));
        assert!(
            answer.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
            "{answer}"
        );
    }

    #[test]
    fn a_request_line_without_its_end_is_answered_when_the_client_stops_sending() {
        let answer = answered(|stream| {
            pieces(stream, &["GET /hea", "lthz"]);
            stream.shutdown(Shutdown::Write).expect("a shutdown");
        });
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    }

    #[test]
    fn a_request_line_longer_than_the_buffer_is_answered_by_what_fits() {
        let started = Instant::now();
        let answer = answered(|stream| {
            let line = format!("GET /{}", "a".repeat(1024 - "GET /".len()));
            stream.write_all(line.as_bytes()).expect("the line");
        });
        assert!(answer.starts_with("HTTP/1.1 404 Not Found\r\n"), "{answer}");
        assert!(started.elapsed() < IO_TIMEOUT, "it waited for more");
    }

    #[test]
    fn a_client_that_never_ends_its_request_line_gets_no_answer() {
        let started = Instant::now();
        let answer = answered(|stream| pieces(stream, &["GET /metrics"]));
        assert_eq!(answer, "", "an answer to half a line");
        let waited = started.elapsed();
        assert!(waited >= IO_TIMEOUT, "closed after {waited:?}");
        assert!(waited < IO_TIMEOUT * 3, "closed after {waited:?}");
    }
}
