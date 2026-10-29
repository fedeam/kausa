//! The output (docs/explanation/architecture.md, "Output"): an `events.k8s.io/v1` Event about the
//! failing workload, `regarding` its top-level owner and `related` the Pod the context came from.
//!
//! The Event's name is derived from the incident's fingerprint and from its note: an Event's note
//! cannot change after it is created (Kubernetes validates it as immutable), so the same incident
//! with the same text updates the existing Event's series, and new text is a new Event. kausa only
//! patches an Event whose reporting controller is `kausa`, and only its series
//! (docs/adr/0009-least-privilege-rbac.md).

use std::time::Duration;

use k8s_openapi::api::core::v1::ObjectReference;
use k8s_openapi::api::events::v1::{Event, EventSeries};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use k8s_openapi::jiff::Timestamp;
use kube::api::{ListParams, Patch, PatchParams, PostParams};
use kube::{Api, Client};
use serde_json::json;

use crate::fingerprint::fingerprint;
use crate::incident::Incident;
use crate::leader::Term;
use crate::leader::pure::WRITE_BOUND;
use crate::prompt::Explanation;
use crate::run::REPORTING_CONTROLLER;

/// The longest note an Event may carry, in bytes (`NoteLengthLimit` in Kubernetes'
/// `pkg/apis/core/validation/events.go`).
pub const MAX_NOTE_BYTES: usize = 1024;

/// What an Event says about an incident.
pub enum Outcome<'a> {
    Explained(&'a Explanation),
    Skipped(&'a str),
}

/// The note of an explanation: the summary, the cause and the steps, cut at a character boundary
/// to fit the limit.
pub fn note(explanation: &Explanation) -> String {
    let mut text = format!(
        "{} Likely cause: {}",
        explanation.summary, explanation.cause
    );
    if !explanation.steps.is_empty() {
        text.push_str(" Check:");
        for (i, step) in explanation.steps.iter().enumerate() {
            text.push_str(&format!(" {}. {}", i + 1, step));
        }
    }
    text.push_str(&format!(" (confidence: {})", explanation.confidence));
    fit(&text)
}

fn fit(text: &str) -> String {
    if text.len() <= MAX_NOTE_BYTES {
        return text.to_owned();
    }
    let ellipsis = "…";
    let mut end = MAX_NOTE_BYTES - ellipsis.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ellipsis}", &text[..end])
}

/// The Event's name: the incident fingerprint, the outcome and eight hexadecimal digits of a hash
/// of the note, valid as a DNS subdomain.
pub fn name(incident_fingerprint: &str, outcome: &Outcome<'_>, note: &str) -> String {
    let note_hash = &fingerprint("", "", "", "", note)[..8];
    match outcome {
        Outcome::Explained(_) => format!("kausa.{incident_fingerprint}.{note_hash}"),
        Outcome::Skipped(_) => format!("kausa.skipped.{incident_fingerprint}.{note_hash}"),
    }
}

/// The merge patch for a repeat: the series only, with the stored count plus one. Everything else
/// of an Event is immutable.
pub fn repeat_patch(stored: &Event, now: Timestamp) -> serde_json::Value {
    let count = stored.series.as_ref().map_or(1, |s| s.count) + 1;
    json!({"series": EventSeries { count, last_observed_time: MicroTime(now) }})
}

/// The Event for an incident.
pub fn build(incident: &Incident, outcome: &Outcome<'_>, instance: &str, now: Timestamp) -> Event {
    let (type_, reason, note) = match outcome {
        Outcome::Explained(explanation) => ("Normal", "Explained", note(explanation)),
        Outcome::Skipped(why) => (
            "Warning",
            "ExplanationSkipped",
            fit(&format!("kausa could not explain this failure: {why}")),
        ),
    };
    let namespace = incident.key.namespace.clone();
    let owner = &incident.owner;
    let regarding = ObjectReference {
        api_version: Some(owner.api_version.clone()),
        kind: Some(owner.kind.clone()),
        name: Some(owner.name.clone()),
        namespace: Some(namespace.clone()),
        uid: Some(owner.uid.clone()).filter(|uid| !uid.is_empty()),
        ..ObjectReference::default()
    };
    let related = incident.sample.pod.as_ref().map(|pod| ObjectReference {
        api_version: Some("v1".into()),
        kind: Some("Pod".into()),
        name: Some(pod.clone()),
        namespace: Some(namespace.clone()),
        ..ObjectReference::default()
    });
    let event_name = name(&incident.fingerprint, outcome, &note);
    Event {
        metadata: ObjectMeta {
            name: Some(event_name),
            namespace: Some(namespace),
            ..ObjectMeta::default()
        },
        event_time: Some(MicroTime(now)),
        action: Some("Explain".into()),
        reason: Some(reason.into()),
        note: Some(note),
        type_: Some(type_.into()),
        regarding: Some(regarding),
        related,
        reporting_controller: Some(REPORTING_CONTROLLER.into()),
        reporting_instance: Some(instance.chars().take(128).collect()),
        ..Event::default()
    }
}

/// The longest one emit may take, all of its calls together: `WRITE_BOUND` (a deposed leader's
/// write ends before a standby may take over, docs/adr/0019-high-availability.md), and a short one
/// in the tests.
#[cfg(not(test))]
const EMIT_TIMEOUT: Duration = WRITE_BOUND;
#[cfg(test)]
const EMIT_TIMEOUT: Duration = Duration::from_secs(1);
const _: () = assert!(EMIT_TIMEOUT.as_nanos() <= WRITE_BOUND.as_nanos());

/// How an emit that did not fail ended.
#[derive(Debug)]
pub enum Emitted {
    /// The Event was created or its series patched.
    Written,
    /// The lease term ended before a call: nothing more was sent. Neither a write nor an error.
    Skipped,
}

/// Writes Events.
pub struct Writer {
    client: Client,
    instance: String,
}

impl Writer {
    pub fn new(client: Client, instance: String) -> Self {
        Self { client, instance }
    }

    /// Creates the Event or, when one with its name exists, reads it (with the `list` kausa is
    /// granted) and patches its series, provided kausa wrote it. Each call is made only while
    /// `term` holds, and the whole emit ends within `EMIT_TIMEOUT`.
    pub async fn emit(
        &self,
        incident: &Incident,
        outcome: &Outcome<'_>,
        now: Timestamp,
        term: &Term,
    ) -> Result<Emitted, String> {
        match tokio::time::timeout(
            EMIT_TIMEOUT,
            self.create_or_patch(incident, outcome, now, term),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(format!(
                "no answer within {} seconds",
                EMIT_TIMEOUT.as_secs()
            )),
        }
    }

    async fn create_or_patch(
        &self,
        incident: &Incident,
        outcome: &Outcome<'_>,
        now: Timestamp,
        term: &Term,
    ) -> Result<Emitted, String> {
        let event = build(incident, outcome, &self.instance, now);
        let namespace = incident.key.namespace.clone();
        let name = event.metadata.name.clone().unwrap_or_default();
        let api = Api::<Event>::namespaced(self.client.clone(), &namespace);
        if !term.holds() {
            return Ok(Emitted::Skipped);
        }
        match api.create(&PostParams::default(), &event).await {
            Ok(_) => Ok(Emitted::Written),
            Err(kube::Error::Api(status)) if status.code == 409 => {
                if !term.holds() {
                    return Ok(Emitted::Skipped);
                }
                let params = ListParams::default().fields(&format!("metadata.name={name}"));
                let stored = api
                    .list(&params)
                    .await
                    .map_err(|e| e.to_string())?
                    .items
                    .into_iter()
                    .next()
                    .ok_or("the Event exists but could not be read")?;
                if stored.reporting_controller.as_deref() != Some(REPORTING_CONTROLLER) {
                    return Err(format!(
                        "an Event named {name} exists and kausa did not write it"
                    ));
                }
                let patch = repeat_patch(&stored, now);
                if !term.holds() {
                    return Ok(Emitted::Skipped);
                }
                api.patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                    .await
                    .map(|_| Emitted::Written)
                    .map_err(|e| e.to_string())
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detection;
    use crate::incident::{IncidentKey, Signal};
    use crate::record::OwnerRef;
    use std::collections::BTreeSet;

    fn incident() -> Incident {
        let owner = OwnerRef {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "api".into(),
            uid: "uid-api".into(),
        };
        let key = IncidentKey {
            namespace: "shop".into(),
            owner_kind: "Deployment".into(),
            owner_name: "api".into(),
            detection: Detection::CrashLoop,
        };
        Incident {
            sample: Signal {
                key: key.clone(),
                owner: owner.clone(),
                observed_at: Timestamp::UNIX_EPOCH,
                event: None,
                pod: Some("api-7d9c8b6f4d-2xkqp".into()),
                container: Some("api".into()),
                image: None,
                reason: "CrashLoopBackOff".into(),
                message: String::new(),
                exit_code: Some(1),
            },
            key,
            owner,
            fingerprint: "5d0c2b7e9a1f4c38".into(),
            first_seen: Timestamp::UNIX_EPOCH,
            last_seen: Timestamp::UNIX_EPOCH,
            pods: BTreeSet::new(),
        }
    }

    fn explanation(steps: usize, long: usize) -> Explanation {
        Explanation {
            summary: format!("api crashes on start{}", "é".repeat(long)),
            cause: "it cannot reach its database".into(),
            steps: (1..=steps).map(|i| format!("check {i}")).collect(),
            confidence: "medium".into(),
        }
    }

    #[test]
    fn the_note_reads_well_and_fits_the_limit() {
        assert_eq!(
            note(&explanation(2, 0)),
            "api crashes on start Likely cause: it cannot reach its database Check: 1. check 1 \
             2. check 2 (confidence: medium)"
        );
        let long = note(&explanation(5, 2000));
        assert!(long.len() <= MAX_NOTE_BYTES, "{}", long.len());
        assert!(long.ends_with('…'));
    }

    #[test]
    fn the_event_is_about_the_owner_and_related_to_the_pod() {
        let e = explanation(1, 0);
        let event = build(
            &incident(),
            &Outcome::Explained(&e),
            "kausa-0",
            Timestamp::UNIX_EPOCH,
        );
        let name = event.metadata.name.as_deref().unwrap_or_default();
        let hash = name
            .strip_prefix("kausa.5d0c2b7e9a1f4c38.")
            .unwrap_or_default();
        assert!(
            hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "{name}"
        );
        assert_eq!(event.metadata.namespace.as_deref(), Some("shop"));
        assert_eq!(event.reason.as_deref(), Some("Explained"));
        assert_eq!(event.type_.as_deref(), Some("Normal"));
        assert_eq!(event.reporting_controller.as_deref(), Some("kausa"));
        let regarding = event.regarding.expect("regarding");
        assert_eq!(regarding.kind.as_deref(), Some("Deployment"));
        assert_eq!(regarding.uid.as_deref(), Some("uid-api"));
        assert_eq!(
            event.related.and_then(|r| r.name).as_deref(),
            Some("api-7d9c8b6f4d-2xkqp")
        );
    }

    #[test]
    fn a_different_note_is_a_different_event_and_a_repeat_patches_only_the_series() {
        let a = explanation(1, 0);
        let mut b = explanation(1, 0);
        b.cause = "other words".into();
        let now = Timestamp::UNIX_EPOCH;
        let ea = build(&incident(), &Outcome::Explained(&a), "k", now);
        let eb = build(&incident(), &Outcome::Explained(&b), "k", now);
        assert_ne!(ea.metadata.name, eb.metadata.name);
        assert_eq!(
            ea.metadata.name,
            build(&incident(), &Outcome::Explained(&a), "k", now)
                .metadata
                .name
        );
        let patch = repeat_patch(&ea, now);
        assert_eq!(
            patch.as_object().map(|o| o.keys().collect::<Vec<_>>()),
            Some(vec![&"series".to_owned()])
        );
        assert_eq!(patch["series"]["count"], 2);
        let mut seen = ea.clone();
        seen.series = Some(EventSeries {
            count: 7,
            last_observed_time: MicroTime(now),
        });
        assert_eq!(repeat_patch(&seen, now)["series"]["count"], 8);
    }

    #[test]
    fn a_skipped_explanation_is_a_warning_with_its_own_name() {
        let event = build(
            &incident(),
            &Outcome::Skipped("the hourly call budget is spent"),
            "kausa-0",
            Timestamp::UNIX_EPOCH,
        );
        let name = event.metadata.name.as_deref().unwrap_or_default();
        let hash = name
            .strip_prefix("kausa.skipped.5d0c2b7e9a1f4c38.")
            .unwrap_or_default();
        assert!(
            hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "{name}"
        );
        assert_eq!(event.type_.as_deref(), Some("Warning"));
        assert_eq!(event.reason.as_deref(), Some("ExplanationSkipped"));
    }

    // Acceptance tests for the standby replica (docs/adr/0019-high-availability.md), written before
    // the code: `Writer::emit` takes the term (`term: &Term`, last) and checks it before each call
    // (the create, the list after a `409` and the series patch); past the term's end it makes no
    // call and returns `Ok(Emitted::Skipped)`, which the caller counts neither as written nor as an
    // error. The whole emit is bounded by `WRITE_BOUND` in every mode (shorter under cfg(test)).

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crate::leader::Term;
    use crate::leader::pure::WRITE_BOUND;

    /// What the fake API server does with one connection.
    enum Reply {
        /// Waits this long, then answers this status line and JSON body.
        After(Duration, &'static str, serde_json::Value),
        /// Reads the request and never answers (the connection stays open).
        Never,
    }

    /// A fake API server: one connection per reply, in order. The request line of each request is
    /// sent back on the channel. Returns `http://127.0.0.1:<port>`.
    fn api_server(replies: Vec<Reply>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            let mut open = Vec::new();
            for reply in replies {
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
                            let line = text.lines().next().unwrap_or_default().to_owned();
                            let _ = sent.send(line);
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                match reply {
                    Reply::After(wait, status, body) => {
                        std::thread::sleep(wait);
                        let body = body.to_string();
                        let answer = format!(
                            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(answer.as_bytes());
                    }
                    Reply::Never => open.push(stream),
                }
            }
            // Keep the unanswered connections open until the test is over.
            std::thread::sleep(Duration::from_secs(60));
            drop(open);
        });
        (url, received)
    }

    fn conflict() -> serde_json::Value {
        json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "an invented message",
            "reason": "AlreadyExists",
            "code": 409,
        })
    }

    /// Emits the explanation of `incident()` against the fake API server at `url` under `term`,
    /// and returns the result and how long it took.
    fn emit_at(url: &str, term: &Term) -> (Result<Emitted, String>, Duration) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(async {
                let config = kube::Config::new(url.parse().expect("a URL"));
                let writer = Writer::new(Client::try_from(config).expect("a client"), "k".into());
                let e = explanation(1, 0);
                let started = Instant::now();
                let outcome = Outcome::Explained(&e);
                let result = tokio::time::timeout(
                    WRITE_BOUND + Duration::from_secs(2),
                    writer.emit(&incident(), &outcome, Timestamp::UNIX_EPOCH, term),
                )
                .await
                .expect("emit is bounded by WRITE_BOUND");
                (result, started.elapsed())
            })
    }

    #[test]
    fn a_closed_term_writes_no_event() {
        let (url, requests) =
            api_server(vec![Reply::After(Duration::ZERO, "201 Created", json!({}))]);
        let (result, _) = emit_at(&url, &Term::closed());
        assert!(matches!(result, Ok(Emitted::Skipped)), "{result:?}");
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "the API server was called"
        );
    }

    #[test]
    fn an_unbounded_term_writes_as_before() {
        let e = explanation(1, 0);
        let created = serde_json::to_value(build(
            &incident(),
            &Outcome::Explained(&e),
            "k",
            Timestamp::UNIX_EPOCH,
        ))
        .expect("an Event");
        let (url, requests) =
            api_server(vec![Reply::After(Duration::ZERO, "201 Created", created)]);
        let (result, _) = emit_at(&url, &Term::unbounded());
        assert!(matches!(result, Ok(Emitted::Written)), "{result:?}");
        let line = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("a create");
        assert!(
            line.starts_with("POST /apis/events.k8s.io/v1/namespaces/shop/events"),
            "{line}"
        );
    }

    #[test]
    fn a_term_that_ends_during_the_create_makes_no_further_call() {
        // The create is answered 409 after the term has ended: no list and no series patch.
        let (url, requests) = api_server(vec![
            Reply::After(Duration::from_millis(300), "409 Conflict", conflict()),
            Reply::After(Duration::ZERO, "200 OK", json!({"items": []})),
        ]);
        let term = Term::closed();
        term.extend_to(Instant::now() + Duration::from_millis(100));
        let (result, _) = emit_at(&url, &term);
        assert!(matches!(result, Ok(Emitted::Skipped)), "{result:?}");
        let line = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("the create");
        assert!(line.starts_with("POST "), "{line}");
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "a call was made after the term ended"
        );
    }

    #[test]
    fn an_emit_against_a_silent_api_server_times_out() {
        let (url, requests) = api_server(vec![Reply::Never]);
        let (result, elapsed) = emit_at(&url, &Term::unbounded());
        assert!(result.is_err(), "an unanswered create is an error");
        assert!(
            elapsed <= WRITE_BOUND + Duration::from_millis(500),
            "{elapsed:?}"
        );
        assert!(requests.recv_timeout(Duration::from_secs(1)).is_ok());
    }
}
