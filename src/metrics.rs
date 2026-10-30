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
pub const COUNTERS: [(&str, &str); 19] = [
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
        "Notifications by final outcome (sent, failed, dropped).",
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
];

/// The gauges kausa exports, with their help text, rendered after the counters
/// (docs/adr/0019-high-availability.md).
pub const GAUGES: [(&str, &str); 1] = [(
    "kausa_leader",
    "1 while this process acts (it holds the Lease, or runs without --leader-lease), 0 as a \
     standby.",
)];

#[derive(Debug)]
pub struct Metrics {
    counters: RefCell<BTreeMap<(&'static str, String), u64>>,
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
            ready: Cell::new(false),
            leading: Cell::new(true),
        }
    }
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
        for (name, help) in GAUGES {
            let value = match name {
                "kausa_leader" => u8::from(self.is_leading()),
                _ => 0,
            };
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
            ));
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
        // The two counters of docs/adr/0019-high-availability.md come after these.
        assert_eq!(COUNTERS.len(), 19);
        assert_eq!(
            COUNTERS[15],
            (
                "kausa_notifications_total",
                "Notifications by final outcome (sent, failed, dropped)."
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
        m.inc("kausa_notifications_retried_total", &[]);
        let text = respond("GET /metrics HTTP/1.1", &m).2;
        for line in [
            "kausa_notifications_total{outcome=\"sent\"} 1\n",
            "kausa_notifications_total{outcome=\"failed\"} 1\n",
            "kausa_notifications_total{outcome=\"dropped\"} 1\n",
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
        // 19: the standby's two counters (docs/adr/0019-high-availability.md) came later.
        assert_eq!(COUNTERS.len(), 19, "a counter was added");
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
        assert_eq!(COUNTERS.len(), 19);
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
        assert_eq!(GAUGES.len(), 1);
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
}
