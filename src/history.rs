//! The explanation history (docs/adr/0014-explanation-history.md): each incident's explanations in
//! an `Explanation` object in its namespace, written after the Event, and swept for age and count.
//! The pure part is in `history/pure.rs`, which the fuzz crate includes by path.

use std::io;
use std::time::Duration;

use k8s_openapi::jiff::Timestamp;
use kube::api::{
    ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams,
    PostParams, Preconditions, ValidationDirective,
};
use kube::{Api, Client};
use serde_json::{Value, json};

use crate::incident::Incident;
use crate::leader::Term;
use crate::leader::pure::WRITE_BOUND;
use crate::log;
use crate::metrics::Metrics;
use crate::prompt::Explanation;

mod pure;

/// The most the read-modify-write of one Explanation may take, all of its calls together, so that
/// a slow API server delays the next explanation by at most this much and never the watches.
pub(crate) const HISTORY_TIMEOUT: Duration = Duration::from_secs(10);
// A deposed leader's write ends before a standby may take over (docs/adr/0019-high-availability.md).
const _: () = assert!(HISTORY_TIMEOUT.as_nanos() <= WRITE_BOUND.as_nanos());
/// How often the sweep is due: at start, and once this has passed since the last one began.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
/// The most one sweep may hold the explanation slot; what is left waits for the next one.
pub(crate) const SWEEP_TIME: Duration = Duration::from_secs(30);
/// The objects asked for in each page of the sweep's list (`limit`, then `continue`). kube buffers
/// a whole page and parses it before kausa sees it, and the schema does not bound an object's
/// `metadata`: an object anyone may create with the label can fill etcd's limit (about 1.5 MiB)
/// with labels, annotations or finalizers, and such an object measured about 12 MiB once parsed.
/// Two per page keeps one page under about 30 MiB at worst.
pub(crate) const SWEEP_PAGE: u32 = 2;

/// `now` cut to whole seconds, and the age cutoff `days` days before it, both as
/// `YYYY-MM-DDTHH:MM:SSZ` (UTC), the one form the pure part compares as strings.
pub(crate) fn times(now: Timestamp, days: u32) -> (String, String) {
    let second = now.as_second();
    let now = Timestamp::from_second(second).unwrap_or(now);
    let cutoff = Timestamp::from_second(second.saturating_sub(i64::from(days) * 86_400))
        .unwrap_or(Timestamp::MIN);
    let format = |time: Timestamp| time.strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    (format(now), format(cutoff))
}

/// The resource, written out: kube's `derive` feature stays off (docs/adr/0014).
fn resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("kausa.fedeam.com", "v1alpha1", "Explanation"),
        "explanations",
    )
}

/// How a write ended: the value of the `outcome` label of `kausa_history_writes_total`, and what
/// the log says when it did not succeed. Nothing here quotes a stored object.
enum Written {
    Created,
    Updated,
    Refused(String),
    Conflict,
    Error(String),
    /// The lease term ended before a call: nothing more was sent. Neither counted nor logged.
    Skipped,
}

impl Written {
    /// The `outcome` label. A skip has one for completeness, but it is never counted.
    fn label(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Refused(_) => "refused",
            Self::Conflict => "conflict",
            Self::Error(_) => "error",
            Self::Skipped => "skipped",
        }
    }

    /// A failed create or patch: a `409` is a conflict (not retried), anything else an error,
    /// logged without the API server's message (`api_failure`).
    fn failed(error: &kube::Error) -> Self {
        let code = match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        };
        match pure::failure_outcome(code) {
            "conflict" => Self::Conflict,
            _ => Self::Error(api_failure(error)),
        }
    }
}

/// Writes the history of each explained incident (only `Explained` outcomes, after the Event and
/// independent of it).
pub struct History {
    client: Client,
    resource: ApiResource,
    /// `--history-days`: entries last written before this many days are dropped.
    days: u32,
}

impl History {
    pub fn new(client: Client, days: u32) -> Self {
        Self {
            client,
            resource: resource(),
            days,
        }
    }

    /// Writes `explanation` into the Explanation of `incident`, counts the outcome and logs a
    /// failure or a refusal. Each call is made only while `term` holds; past its end the write
    /// stops silently. Nothing stops on a failure; only an unwritable log is returned.
    pub async fn write(
        &self,
        incident: &Incident,
        explanation: &Explanation,
        model: &str,
        metrics: &Metrics,
        term: &Term,
    ) -> io::Result<()> {
        let (now, cutoff) = times(Timestamp::now(), self.days);
        let written = tokio::time::timeout(
            HISTORY_TIMEOUT,
            self.read_modify_write_in(term, incident, explanation, model, &now, &cutoff),
        )
        .await
        .unwrap_or_else(|_| {
            Written::Error(format!(
                "no answer within {} seconds",
                HISTORY_TIMEOUT.as_secs()
            ))
        });
        if !matches!(written, Written::Skipped) {
            metrics.inc(
                "kausa_history_writes_total",
                &[("outcome", written.label())],
            );
        }
        let about = |key: &str, value: &str| {
            json!({
                "fingerprint": incident.fingerprint,
                "namespace": incident.key.namespace,
                key: value,
            })
        };
        match &written {
            Written::Created | Written::Updated | Written::Skipped => Ok(()),
            Written::Refused(reason) => log::warn("history write refused", about("reason", reason)),
            Written::Conflict => log::warn("history write failed", about("error", pure::CONFLICT)),
            Written::Error(error) => log::warn("history write failed", about("error", error)),
        }
    }

    /// One read-modify-write: a `list` by name; a `create` when there is no object; otherwise the
    /// identity check and a merge patch of `status` with the `resourceVersion` read. Each call
    /// only while `term` holds.
    async fn read_modify_write_in(
        &self,
        term: &Term,
        incident: &Incident,
        explanation: &Explanation,
        model: &str,
        now: &str,
        cutoff: &str,
    ) -> Written {
        let fingerprint = incident.fingerprint.as_str();
        let name = pure::name(fingerprint);
        let owner = &incident.owner;
        let spec = pure::spec_of(
            incident.key.detection.id(),
            &owner.api_version,
            &owner.kind,
            &owner.name,
        );
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), &incident.key.namespace, &self.resource);
        let params = ListParams::default().fields(&format!("metadata.name={name}"));
        if !term.holds() {
            return Written::Skipped;
        }
        let listed = match api.list(&params).await {
            Ok(list) => list.items.into_iter().next(),
            Err(error) => return Written::Error(list_failure(&error)),
        };
        // The stored object is reduced at once (invariant 8) and is untrusted (invariant 5).
        let stored = match listed.map(|object| serde_json::to_value(&object)) {
            None => None,
            Some(Ok(object)) => Some(pure::parse_stored(&object)),
            Some(Err(error)) => return Written::Error(error.to_string()),
        };
        let Some(stored) = stored else {
            let body = pure::create_body(fingerprint, &spec, explanation, model, now);
            let object: DynamicObject = match serde_json::from_value(body) {
                Ok(object) => object,
                Err(error) => return Written::Error(error.to_string()),
            };
            if !term.holds() {
                return Written::Skipped;
            }
            return match api.create(&PostParams::default(), &object).await {
                Ok(_) => Written::Created,
                Err(error) => Written::failed(&error),
            };
        };
        if let Err(reason) = pure::check_identity(&stored, fingerprint, &spec) {
            return Written::Refused(reason);
        }
        let Some(resource_version) = stored.resource_version.as_deref() else {
            return Written::Error(format!(
                "the Explanation {name} was listed without a resourceVersion"
            ));
        };
        let status = pure::merge(&stored, explanation, model, now, cutoff);
        let patch = pure::patch_body(resource_version, &status);
        let params = PatchParams {
            field_validation: Some(ValidationDirective::Strict),
            ..PatchParams::default()
        };
        if !term.holds() {
            return Written::Skipped;
        }
        match api.patch(&name, &params, &Patch::Merge(&patch)).await {
            Ok(_) => Written::Updated,
            Err(error) => Written::failed(&error),
        }
    }

    /// `read_modify_write_in` under a term that never ends, as kausa runs without
    /// `--leader-lease`.
    #[cfg(test)]
    async fn read_modify_write(
        &self,
        incident: &Incident,
        explanation: &Explanation,
        model: &str,
        now: &str,
        cutoff: &str,
    ) -> Written {
        self.read_modify_write_in(
            &Term::unbounded(),
            incident,
            explanation,
            model,
            now,
            cutoff,
        )
        .await
    }

    /// One sweep: deletes the Explanations older than `--history-days` and, oldest first, those
    /// beyond `MAX_OBJECTS`. It lists the labelled objects in pages of `SWEEP_PAGE`, across the
    /// cluster when `namespaces` is empty or in each listed namespace in order, deletes as each page
    /// arrives, each delete with the `resourceVersion` listed as a precondition, and stops at
    /// `SWEEP_TIME`. A failed list ends its namespace and the sweep goes on with the next; a delete
    /// answered `404` or `409` (gone, or written since the list) is neither an error nor a deletion.
    /// Each call is made only while `term` holds (the sweep ends silently past its end) and ends
    /// within `WRITE_BOUND`, or at the sweep's deadline if that comes first; a call that does not
    /// end in time ends the sweep. Nothing stops on a failure; only an unwritable log is returned.
    pub async fn sweep(
        &self,
        namespaces: &[String],
        metrics: &Metrics,
        term: &Term,
    ) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + SWEEP_TIME;
        let call_end = || deadline.min(tokio::time::Instant::now() + WRITE_BOUND);
        let (now, cutoff) = times(Timestamp::now(), self.days);
        let mut plan = pure::SweepPlan::new(&now, &cutoff);
        let (mut by_age, mut by_cap) = (0_u64, 0_u64);
        let scopes: Vec<Option<&str>> = if namespaces.is_empty() {
            vec![None]
        } else {
            namespaces.iter().map(|n| Some(n.as_str())).collect()
        };
        let selector = format!("{}={}", pure::LABEL, pure::MANAGER);
        'scopes: for scope in scopes {
            // The log names the namespace of the flag (null in cluster scope), never a listed one.
            let failed = |error: String| {
                metrics.inc("kausa_history_sweep_errors_total", &[]);
                log::warn(
                    "history sweep failed",
                    json!({"namespace": scope, "error": error}),
                )
            };
            let api: Api<DynamicObject> = match scope {
                None => Api::all_with(self.client.clone(), &self.resource),
                Some(namespace) => {
                    Api::namespaced_with(self.client.clone(), namespace, &self.resource)
                }
            };
            let mut token: Option<String> = None;
            loop {
                let mut params = ListParams::default().labels(&selector).limit(SWEEP_PAGE);
                if let Some(token) = &token {
                    params = params.continue_token(token);
                }
                if !term.holds() {
                    break 'scopes;
                }
                let Ok(listed) = tokio::time::timeout_at(call_end(), api.list(&params)).await
                else {
                    break 'scopes;
                };
                let list = match listed {
                    Ok(list) => list,
                    Err(error) => {
                        failed(list_failure(&error))?;
                        continue 'scopes;
                    }
                };
                token = list.metadata.continue_.filter(|token| !token.is_empty());
                // Each object is reduced at once (invariant 8) and is untrusted (invariant 5).
                let items: Vec<Value> = list
                    .items
                    .into_iter()
                    .filter_map(|object| serde_json::to_value(without_unread_metadata(object)).ok())
                    .collect();
                let deletes = plan.page(&items);
                drop(items);
                for delete in deletes {
                    let api: Api<DynamicObject> = Api::namespaced_with(
                        self.client.clone(),
                        &delete.namespace,
                        &self.resource,
                    );
                    let params = DeleteParams {
                        preconditions: Some(Preconditions {
                            resource_version: Some(delete.resource_version.clone()),
                            uid: None,
                        }),
                        ..DeleteParams::default()
                    };
                    if !term.holds() {
                        break 'scopes;
                    }
                    let Ok(answer) =
                        tokio::time::timeout_at(call_end(), api.delete(&delete.name, &params))
                            .await
                    else {
                        break 'scopes;
                    };
                    match answer {
                        Ok(_) => {
                            metrics.inc(
                                "kausa_history_deleted_total",
                                &[("reason", delete.reason.label())],
                            );
                            match delete.reason {
                                pure::Reason::Age => by_age += 1,
                                pure::Reason::Cap => by_cap += 1,
                            }
                        }
                        Err(kube::Error::Api(status)) if matches!(status.code, 404 | 409) => {}
                        Err(error) => failed(api_failure(&error))?,
                    }
                }
                if token.is_none() {
                    break;
                }
            }
        }
        if by_age + by_cap > 0 {
            log::info(
                "history swept",
                json!({"deleted_by_age": by_age, "deleted_by_cap": by_cap}),
            )?;
        }
        Ok(())
    }
}

/// A listed object without the `metadata` the sweep never reads and the schema does not bound:
/// annotations, labels other than kausa's, finalizers, managed fields and owner references. Its
/// copy as JSON is then as small as the schema allows, whatever the object carried.
fn without_unread_metadata(mut object: DynamicObject) -> DynamicObject {
    let metadata = &mut object.metadata;
    metadata.annotations = None;
    metadata.finalizers = None;
    metadata.managed_fields = None;
    metadata.owner_references = None;
    metadata.labels = metadata.labels.take().map(|labels| {
        labels
            .into_iter()
            .filter(|(key, _)| key == pure::LABEL)
            .collect()
    });
    object
}

/// What the log says about a failed `list`: a `404` means the CRD is not installed; anything else
/// is told as `api_failure` tells it.
fn list_failure(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) if status.code == 404 => pure::CRD_MISSING.to_owned(),
        _ => api_failure(error),
    }
}

/// What the log says about a failed request: for an answer of the API server, its code and reason
/// only. Its message is left out: a `422` quotes the whole stored object (seen in kind), an
/// admission webhook's can quote the object, and in cluster scope a refused delete names the
/// listed object's namespace; nothing read back from an Explanation may reach the log
/// (invariant 5). Any other failure keeps kube-rs's text.
fn api_failure(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!(
            "{} {} (the API server's message is not logged: it can quote the stored object)",
            status.code, status.reason
        ),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::jiff::Timestamp;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::leader::Term;
    use crate::leader::pure::WRITE_BOUND;

    fn is_time(time: &str) -> bool {
        regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
            .expect("a valid pattern")
            .is_match(time)
    }

    #[test]
    fn the_limits_of_the_writer_and_the_sweep() {
        assert_eq!(HISTORY_TIMEOUT, Duration::from_secs(10));
        assert_eq!(SWEEP_INTERVAL, Duration::from_secs(3600));
        assert_eq!(SWEEP_TIME, Duration::from_secs(30));
        assert_eq!(SWEEP_PAGE, 2);
    }

    #[test]
    fn now_is_cut_to_whole_seconds_and_the_cutoff_is_whole_days_before() {
        let now: Timestamp = "2026-10-28T10:00:00.987654321Z".parse().expect("a time");
        assert_eq!(
            times(now, 7),
            (
                "2026-10-28T10:00:00Z".to_owned(),
                "2026-10-21T10:00:00Z".to_owned()
            )
        );
        assert_eq!(times(now, 1).1, "2026-10-27T10:00:00Z");
        assert_eq!(times(now, 30).1, "2026-09-28T10:00:00Z");
        let exact: Timestamp = "2026-03-01T00:00:00Z".parse().expect("a time");
        assert_eq!(times(exact, 1).1, "2026-02-28T00:00:00Z");
        let (now, cutoff) = times(Timestamp::now(), 7);
        assert!(is_time(&now), "{now}");
        assert!(is_time(&cutoff), "{cutoff}");
        assert!(cutoff < now);
    }

    fn explanation() -> Explanation {
        Explanation {
            summary: "The container exits with code 1 at start.".into(),
            cause: "The setting it reads at start is missing.".into(),
            steps: vec!["Check the Deployment's environment.".into()],
            confidence: "medium".into(),
        }
    }

    fn spec() -> pure::Spec {
        pure::spec_of("D001", "apps/v1", "Deployment", "api")
    }

    #[test]
    fn the_resource_is_written_out() {
        let resource = resource();
        assert_eq!(resource.api_version, "kausa.fedeam.com/v1alpha1");
        assert_eq!(resource.group, "kausa.fedeam.com");
        assert_eq!(resource.version, "v1alpha1");
        assert_eq!(resource.kind, "Explanation");
        assert_eq!(resource.plural, "explanations");
    }

    #[test]
    fn the_create_body_is_sent_as_built() {
        // `create` takes a DynamicObject: what it serializes is exactly the pure body, with no
        // namespace, owner reference or other metadata added.
        let body = pure::create_body(
            "0123456789abcdef",
            &spec(),
            &explanation(),
            "m",
            "2026-10-28T10:00:00Z",
        );
        let object: DynamicObject = serde_json::from_value(body.clone()).expect("an object");
        assert_eq!(serde_json::to_value(&object).expect("JSON"), body);
    }

    #[test]
    fn a_listed_object_keeps_what_the_write_reads() {
        let mut listed = pure::create_body(
            "0123456789abcdef",
            &spec(),
            &explanation(),
            "m",
            "2026-10-28T10:00:00Z",
        );
        listed["metadata"]["namespace"] = json!("shop");
        listed["metadata"]["resourceVersion"] = json!("4711");
        listed["metadata"]["uid"] = json!("an-invented-uid");
        listed["metadata"]["creationTimestamp"] = json!("2026-10-28T10:00:00Z");
        let object: DynamicObject = serde_json::from_value(listed).expect("an object");
        let stored = pure::parse_stored(&serde_json::to_value(&object).expect("JSON"));
        assert!(stored.labelled);
        assert_eq!(stored.spec, Some(spec()));
        assert_eq!(stored.resource_version.as_deref(), Some("4711"));
        assert_eq!(stored.entries.len(), 1);
        assert_eq!(stored.occurrences, Some(1));
        assert_eq!(
            pure::check_identity(&stored, "0123456789abcdef", &spec()),
            Ok(())
        );
    }

    #[test]
    fn a_failed_list_names_the_missing_crd_only_for_a_404() {
        let api = |code: u16| {
            kube::Error::Api(Box::new(
                kube::core::Status::failure("an invented message", "an invented reason")
                    .with_code(code),
            ))
        };
        assert_eq!(list_failure(&api(404)), pure::CRD_MISSING);
        for code in [403, 410, 500] {
            let failure = list_failure(&api(code));
            // The message can name a listed namespace or quote an object, so it is never logged.
            assert!(
                !failure.contains("an invented message"),
                "{code}: {failure}"
            );
            assert!(
                failure.starts_with(&format!("{code} an invented reason ")),
                "{code}: {failure}"
            );
        }
    }

    /// A request the fake API server received.
    struct Sent {
        /// The request line: method, path and query.
        line: String,
        /// The header lines, lowercased.
        head: String,
        body: String,
    }

    impl Sent {
        fn json(&self) -> Value {
            serde_json::from_str(&self.body).expect("a JSON body")
        }
    }

    /// A fake API server: one connection per request, answered in order from `answers` (a status
    /// line and a JSON body). Each request is sent back on the channel.
    fn api_server(answers: Vec<(&'static str, Value)>) -> (String, mpsc::Receiver<Sent>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().expect("a connection");
                let mut request = Vec::new();
                let mut buffer = vec![0; 65536];
                loop {
                    let n = stream.read(&mut buffer).expect("a read");
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
                let body = body.to_string();
                let answer = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(answer.as_bytes()).expect("a write");
            }
        });
        (url, received)
    }

    fn sweep(url: &str, namespaces: &[&str]) -> Metrics {
        let namespaces: Vec<String> = namespaces.iter().map(|&n| n.to_owned()).collect();
        let metrics = Metrics::default();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(async {
                let config = kube::Config::new(url.parse().expect("a URL"));
                let client = Client::try_from(config).expect("a client");
                History::new(client, 7)
                    .sweep(&namespaces, &metrics, &Term::unbounded())
                    .await
                    .expect("a log");
            });
        metrics
    }

    fn page(items: Vec<Value>, token: &str) -> Value {
        json!({
            "apiVersion": "kausa.fedeam.com/v1alpha1",
            "kind": "ExplanationList",
            "metadata": {"continue": token},
            "items": items,
        })
    }

    /// A listed object last written in 2000, so past any `--history-days`.
    fn old(namespace: &str, fingerprint: &str, resource_version: &str) -> Value {
        let mut object = pure::create_body(
            fingerprint,
            &spec(),
            &explanation(),
            "m",
            "2000-01-01T00:00:00Z",
        );
        object["metadata"]["namespace"] = json!(namespace);
        object["metadata"]["resourceVersion"] = json!(resource_version);
        object["metadata"]["creationTimestamp"] = json!("2000-01-01T00:00:00Z");
        object
    }

    fn failure(code: u16) -> Value {
        json!({
            "apiVersion": "v1",
            "kind": "Status",
            "metadata": {},
            "status": "Failure",
            "message": "an invented message",
            "reason": "an invented reason",
            "code": code,
        })
    }

    /// The value of one series in `/metrics`, 0 when it was never incremented (the counter is
    /// then rendered without it).
    fn counter(metrics: &Metrics, series: &str) -> u64 {
        let text = metrics.render();
        let name = series.split('{').next().unwrap_or(series);
        assert!(text.contains(&format!("# TYPE {name} counter\n")), "{text}");
        text.lines()
            .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
            .map_or(0, |value| value.parse().expect("a count"))
    }

    const PLURAL: &str = "/apis/kausa.fedeam.com/v1alpha1";
    const SELECTOR: &str = "labelSelector=app.kubernetes.io%2Fmanaged-by%3Dkausa";

    #[test]
    fn the_cluster_sweep_pages_the_labelled_objects_and_deletes_with_preconditions() {
        let mut foreign = old("shop", "0123456789abcdef", "10");
        foreign["metadata"]["name"] = json!("not-kausa");
        let (url, requests) = api_server(vec![
            (
                "200 OK",
                page(vec![old("shop", "0123456789abcdef", "11"), foreign], "c1"),
            ),
            ("200 OK", old("shop", "0123456789abcdef", "11")),
            (
                "200 OK",
                page(vec![old("web", "fedcba9876543210", "12")], ""),
            ),
            ("409 Conflict", failure(409)),
        ]);
        let metrics = sweep(&url, &[]);
        let requests: Vec<Sent> = requests.try_iter().collect();
        let lines: Vec<&str> = requests.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(requests.len(), 4, "{lines:#?}");
        let line = |i: usize| requests[i].line.clone();
        assert!(
            line(0).starts_with(&format!("GET {PLURAL}/explanations?")),
            "{}",
            line(0)
        );
        assert!(
            line(0).contains(SELECTOR) && line(0).contains("limit=2"),
            "{}",
            line(0)
        );
        assert!(!line(0).contains("continue="), "{}", line(0));
        assert!(
            line(1).starts_with(&format!(
                "DELETE {PLURAL}/namespaces/shop/explanations/kausa.0123456789abcdef"
            )),
            "{}",
            line(1)
        );
        let body = |i: usize| requests[i].json();
        assert_eq!(body(1)["preconditions"], json!({"resourceVersion": "11"}));
        assert!(
            line(2).contains("continue=c1") && line(2).contains(SELECTOR),
            "{}",
            line(2)
        );
        assert!(line(2).contains("limit=2"), "{}", line(2));
        assert!(
            line(3).starts_with(&format!(
                "DELETE {PLURAL}/namespaces/web/explanations/kausa.fedcba9876543210"
            )),
            "{}",
            line(3)
        );
        assert_eq!(body(3)["preconditions"], json!({"resourceVersion": "12"}));
        // The 409 (written since the list) is neither a deletion nor an error.
        assert_eq!(
            counter(&metrics, "kausa_history_deleted_total{reason=\"age\"}"),
            1
        );
        assert_eq!(
            counter(&metrics, "kausa_history_deleted_total{reason=\"cap\"}"),
            0
        );
        assert_eq!(counter(&metrics, "kausa_history_sweep_errors_total"), 0);
    }

    #[test]
    fn a_failed_list_ends_its_namespace_and_the_sweep_goes_on() {
        let (url, requests) = api_server(vec![
            ("404 Not Found", failure(404)),
            (
                "200 OK",
                page(
                    vec![
                        old("b", "0123456789abcdef", "5"),
                        old("b", "fedcba9876543210", "6"),
                    ],
                    "t1",
                ),
            ),
            ("500 Internal Server Error", failure(500)),
            ("404 Not Found", failure(404)),
            ("410 Gone", failure(410)),
            ("200 OK", page(vec![], "")),
        ]);
        let metrics = sweep(&url, &["a", "b", "c"]);
        let lines: Vec<String> = requests.try_iter().map(|r| r.line).collect();
        let starts = [
            format!("GET {PLURAL}/namespaces/a/explanations?"),
            format!("GET {PLURAL}/namespaces/b/explanations?"),
            format!("DELETE {PLURAL}/namespaces/b/explanations/kausa.0123456789abcdef"),
            format!("DELETE {PLURAL}/namespaces/b/explanations/kausa.fedcba9876543210"),
            format!("GET {PLURAL}/namespaces/b/explanations?"),
            format!("GET {PLURAL}/namespaces/c/explanations?"),
        ];
        assert_eq!(lines.len(), starts.len(), "{lines:#?}");
        for (line, start) in lines.iter().zip(&starts) {
            assert!(line.starts_with(start.as_str()), "{line}");
        }
        assert!(lines[4].contains("continue=t1"), "{}", lines[4]);
        // The 404 of a's list, the 500 of a delete and the 410 of b's second page (an expired
        // continue token, which ends b); the delete answered 404 (gone) is neither a deletion nor
        // an error.
        assert_eq!(counter(&metrics, "kausa_history_sweep_errors_total"), 3);
        assert_eq!(
            counter(&metrics, "kausa_history_deleted_total{reason=\"age\"}"),
            0
        );
    }

    /// Set in the child copy of the test binary that runs the sweep of
    /// `a_refused_delete_is_logged_without_the_api_servers_message`.
    const SWEEP_CHILD: &str = "KAUSA_TEST_SWEEP_CHILD";
    const MARKER: &str = "an-invented-marker";

    /// kausa's log is written straight to standard output, which the test harness does not
    /// capture, so the sweep runs in a child copy of this test binary and its log is read here.
    #[test]
    fn a_refused_delete_is_logged_without_the_api_servers_message() {
        if std::env::var_os(SWEEP_CHILD).is_some() {
            // A 403 and a 500 whose message names the listed object's namespace and quotes it,
            // as an admission webhook's can.
            let refused = |status: &'static str, code: u16| {
                let mut answer = failure(code);
                answer["message"] = json!(format!(
                    "{MARKER}: explanations.kausa.fedeam.com in namespace shop: {}",
                    old("shop", "0123456789abcdef", "5")
                ));
                (status, answer)
            };
            let (url, _) = api_server(vec![
                (
                    "200 OK",
                    page(
                        vec![
                            old("shop", "0123456789abcdef", "5"),
                            old("shop", "fedcba9876543210", "6"),
                        ],
                        "",
                    ),
                ),
                refused("403 Forbidden", 403),
                refused("500 Internal Server Error", 500),
            ]);
            let metrics = sweep(&url, &[]);
            assert_eq!(counter(&metrics, "kausa_history_sweep_errors_total"), 2);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "history::tests::a_refused_delete_is_logged_without_the_api_servers_message",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SWEEP_CHILD, "1")
            .output()
            .expect("the child test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The harness's `test <name> ... ` can start the line a log line is written on.
        let lines: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.find("{\"ts\":").map(|start| &line[start..]))
            .filter(|line| line.contains("history sweep failed"))
            .collect();
        assert_eq!(lines.len(), 2, "{stdout}");
        for (line, code) in lines.iter().zip([403, 500]) {
            assert!(!line.contains(MARKER), "{line}");
            assert!(!line.contains("shop"), "{line}");
            let fields: Value = serde_json::from_str(line).expect("a JSON line");
            assert_eq!(fields["msg"], "history sweep failed", "{line}");
            // Cluster scope: the namespace of the flag, null, never the listed one.
            assert_eq!(fields["namespace"], Value::Null, "{line}");
            let error = fields["error"].as_str().expect("an error");
            assert!(
                error.starts_with(&format!("{code} an invented reason ")),
                "{line}"
            );
        }
    }

    #[test]
    fn a_conflict_is_told_apart_from_other_failures() {
        let api = |code: u16| {
            kube::Error::Api(Box::new(
                kube::core::Status::failure("an invented message", "an invented reason")
                    .with_code(code),
            ))
        };
        assert_eq!(Written::failed(&api(409)).label(), "conflict");
        for code in [400, 403, 404, 410, 422, 500] {
            let written = Written::failed(&api(code));
            assert_eq!(written.label(), "error", "{code}");
            let Written::Error(error) = &written else {
                unreachable!()
            };
            // The message can quote the stored object (a 422 does), so it is never logged.
            assert!(!error.contains("an invented message"), "{code}: {error}");
            assert!(
                error.starts_with(&format!("{code} an invented reason ")),
                "{code}: {error}"
            );
        }
        assert_eq!(Written::Created.label(), "created");
        assert_eq!(Written::Updated.label(), "updated");
        assert_eq!(Written::Refused(String::new()).label(), "refused");
    }

    /// A listed object near etcd's limit (about 1.4 MB of JSON), made large with what the schema
    /// does not bound: thousands of labels and finalizers, a large annotation and managed fields.
    fn large(namespace: &str, fingerprint: &str, resource_version: &str) -> Value {
        let mut object = old(namespace, fingerprint, resource_version);
        let metadata = &mut object["metadata"];
        for i in 0..30_000 {
            metadata["labels"][format!("example.com/label-{i}")] = json!("x");
        }
        metadata["annotations"] = json!({"example.com/large": "a".repeat(256 * 1024)});
        metadata["finalizers"] = (0..5_000)
            .map(|i| json!(format!("example.com/finalizer-{i}")))
            .collect();
        metadata["managedFields"] = json!([{
            "manager": "e2e",
            "operation": "Update",
            "apiVersion": "kausa.fedeam.com/v1alpha1",
            "fieldsType": "FieldsV1",
            "fieldsV1": {"f:metadata": {"f:annotations": {"f:example.com/large": {}}}},
        }]);
        object
    }

    #[test]
    fn a_large_listed_object_is_reduced_to_what_the_sweep_reads() {
        let listed = large("shop", "0123456789abcdef", "11");
        assert!(listed.to_string().len() > 1_000_000);
        let object: DynamicObject = serde_json::from_value(listed).expect("an object");
        let reduced = serde_json::to_value(without_unread_metadata(object)).expect("JSON");
        assert_eq!(
            reduced["metadata"],
            json!({
                "name": "kausa.0123456789abcdef",
                "namespace": "shop",
                "resourceVersion": "11",
                "creationTimestamp": "2000-01-01T00:00:00Z",
                "labels": {pure::LABEL: pure::MANAGER},
            })
        );
        assert!(reduced.to_string().len() < 2_000, "{reduced}");
        // The plan still finds it kausa-shaped and past the age.
        let mut plan = pure::SweepPlan::new("2026-10-28T10:00:00Z", "2026-10-21T10:00:00Z");
        let deletes = plan.page(&[reduced]);
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].reason, pure::Reason::Age);
        assert_eq!(deletes[0].resource_version, "11");
    }

    #[test]
    fn a_page_of_large_labelled_objects_is_swept() {
        let (url, requests) = api_server(vec![
            (
                "200 OK",
                page(
                    vec![
                        large("shop", "0123456789abcdef", "11"),
                        large("shop", "fedcba9876543210", "12"),
                    ],
                    "",
                ),
            ),
            ("200 OK", old("shop", "0123456789abcdef", "11")),
            ("200 OK", old("shop", "fedcba9876543210", "12")),
        ]);
        let metrics = sweep(&url, &[]);
        let lines: Vec<String> = requests.try_iter().map(|r| r.line).collect();
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[0].contains("limit=2"), "{}", lines[0]);
        for (line, fingerprint) in lines[1..]
            .iter()
            .zip(["0123456789abcdef", "fedcba9876543210"])
        {
            let start = format!("DELETE {PLURAL}/namespaces/shop/explanations/kausa.{fingerprint}");
            assert!(line.starts_with(&start), "{line}");
        }
        assert_eq!(
            counter(&metrics, "kausa_history_deleted_total{reason=\"age\"}"),
            2
        );
        assert_eq!(counter(&metrics, "kausa_history_sweep_errors_total"), 0);
    }

    const NOW: &str = "2026-10-28T10:00:00Z";
    const CUTOFF: &str = "2026-10-21T10:00:00Z";
    const FINGERPRINT: &str = "0123456789abcdef";
    const LIST: &str = "GET /apis/kausa.fedeam.com/v1alpha1/namespaces/shop/explanations?";
    const OBJECT: &str =
        "/apis/kausa.fedeam.com/v1alpha1/namespaces/shop/explanations/kausa.0123456789abcdef";

    /// An incident in `shop` whose `spec` is `spec()`.
    fn incident() -> Incident {
        use crate::detect::Detection;
        use crate::incident::{IncidentKey, Signal};
        use crate::record::OwnerRef;
        let owner = OwnerRef {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "api".into(),
            uid: "an-invented-uid".into(),
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
            fingerprint: FINGERPRINT.into(),
            first_seen: Timestamp::UNIX_EPOCH,
            last_seen: Timestamp::UNIX_EPOCH,
            pods: std::collections::BTreeSet::new(),
        }
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    fn history(url: &str) -> History {
        let config = kube::Config::new(url.parse().expect("a URL"));
        History::new(Client::try_from(config).expect("a client"), 7)
    }

    /// One read-modify-write of `incident()` against the fake API server, at `NOW`.
    fn write_at(url: &str) -> Written {
        block_on(async {
            history(url)
                .read_modify_write(&incident(), &explanation(), "m", NOW, CUTOFF)
                .await
        })
    }

    /// The Explanation of `incident()` as a list returns it, last written the day before `NOW`.
    fn listed(resource_version: &str) -> Value {
        let mut object = pure::create_body(
            FINGERPRINT,
            &spec(),
            &explanation(),
            "m",
            "2026-10-27T10:00:00Z",
        );
        object["metadata"]["namespace"] = json!("shop");
        object["metadata"]["resourceVersion"] = json!(resource_version);
        object["metadata"]["uid"] = json!("an-invented-uid");
        object["metadata"]["creationTimestamp"] = json!("2026-10-27T10:00:00Z");
        object
    }

    fn assert_listed_by_name(sent: &Sent) {
        assert!(sent.line.starts_with(LIST), "{}", sent.line);
        assert!(
            sent.line
                .contains("fieldSelector=metadata.name%3Dkausa.0123456789abcdef"),
            "{}",
            sent.line
        );
    }

    #[test]
    fn a_first_write_lists_by_name_then_creates_the_pure_body() {
        let body = pure::create_body(FINGERPRINT, &spec(), &explanation(), "m", NOW);
        let (url, requests) = api_server(vec![
            ("200 OK", page(vec![], "")),
            ("201 Created", body.clone()),
        ]);
        let written = write_at(&url);
        assert_eq!(written.label(), "created");
        let requests: Vec<Sent> = requests.try_iter().collect();
        assert_eq!(requests.len(), 2);
        assert_listed_by_name(&requests[0]);
        assert!(
            requests[1]
                .line
                .starts_with("POST /apis/kausa.fedeam.com/v1alpha1/namespaces/shop/explanations?"),
            "{}",
            requests[1].line
        );
        assert!(
            requests[1].head.contains("content-type: application/json"),
            "{}",
            requests[1].head
        );
        assert_eq!(requests[1].json(), body);
    }

    #[test]
    fn a_later_write_patches_status_and_resource_version_with_strict_validation() {
        let stored = listed("4711");
        let status = pure::merge(
            &pure::parse_stored(&stored),
            &explanation(),
            "m",
            NOW,
            CUTOFF,
        );
        let patch = pure::patch_body("4711", &status);
        let (url, requests) = api_server(vec![
            ("200 OK", page(vec![stored], "")),
            ("200 OK", listed("4712")),
        ]);
        let written = write_at(&url);
        assert_eq!(written.label(), "updated");
        let requests: Vec<Sent> = requests.try_iter().collect();
        assert_eq!(requests.len(), 2);
        assert_listed_by_name(&requests[0]);
        let line = &requests[1].line;
        assert!(line.starts_with(&format!("PATCH {OBJECT}?")), "{line}");
        assert!(line.contains("fieldValidation=Strict"), "{line}");
        assert!(!line.contains("dryRun"), "{line}");
        assert!(
            requests[1]
                .head
                .contains("content-type: application/merge-patch+json"),
            "{}",
            requests[1].head
        );
        let sent = requests[1].json();
        assert_eq!(sent, patch);
        // Only the resourceVersion read and the status; the same text counts again.
        assert_eq!(sent["metadata"], json!({"resourceVersion": "4711"}));
        assert_eq!(sent.as_object().map(serde_json::Map::len), Some(2));
        assert_eq!(sent["status"]["occurrences"], json!(2));
        assert_eq!(sent["status"]["lastTime"], json!(NOW));
        assert_eq!(sent["status"]["entries"][0]["count"], json!(2));
        assert_eq!(
            sent["status"]["entries"][0]["firstTime"],
            json!("2026-10-27T10:00:00Z")
        );
        assert_eq!(sent["status"]["entries"][0]["lastTime"], json!(NOW));
    }

    #[test]
    fn an_object_without_the_label_is_refused_without_a_write() {
        let mut stored = listed("4711");
        stored["metadata"]
            .as_object_mut()
            .expect("metadata")
            .remove("labels");
        let (url, requests) = api_server(vec![("200 OK", page(vec![stored], ""))]);
        let written = write_at(&url);
        assert_eq!(written.label(), "refused");
        let Written::Refused(reason) = &written else {
            unreachable!()
        };
        assert!(reason.contains("does not carry the label"), "{reason}");
        let requests: Vec<Sent> = requests.try_iter().collect();
        assert_eq!(requests.len(), 1);
        assert_listed_by_name(&requests[0]);
    }

    #[test]
    fn a_patch_answered_409_is_a_conflict() {
        let (url, requests) = api_server(vec![
            ("200 OK", page(vec![listed("4711")], "")),
            ("409 Conflict", failure(409)),
        ]);
        assert_eq!(write_at(&url).label(), "conflict");
        let requests: Vec<Sent> = requests.try_iter().collect();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1].line.starts_with("PATCH "),
            "{}",
            requests[1].line
        );
    }

    #[test]
    fn a_list_answered_404_names_the_missing_crd() {
        let (url, requests) = api_server(vec![("404 Not Found", failure(404))]);
        let written = write_at(&url);
        let Written::Error(error) = &written else {
            panic!("{}", written.label())
        };
        assert_eq!(error, pure::CRD_MISSING);
        assert_eq!(requests.try_iter().count(), 1);
    }

    #[test]
    fn a_write_counts_its_outcome() {
        let (url, _requests) = api_server(vec![
            ("200 OK", page(vec![listed("4711")], "")),
            ("200 OK", listed("4712")),
        ]);
        let metrics = Metrics::default();
        block_on(async {
            history(&url)
                .write(
                    &incident(),
                    &explanation(),
                    "m",
                    &metrics,
                    &Term::unbounded(),
                )
                .await
                .expect("a log");
        });
        assert_eq!(
            counter(&metrics, "kausa_history_writes_total{outcome=\"updated\"}"),
            1
        );
        assert_eq!(
            counter(&metrics, "kausa_history_writes_total{outcome=\"error\"}"),
            0
        );
    }

    // Acceptance tests for the standby replica (docs/adr/0019-high-availability.md), written before
    // the code: `History::write` and `History::sweep` take the term (`term: &Term`, last). Past its
    // end, the write makes no further call and ends as `Written::Skipped`, neither counted nor
    // logged, and the sweep ends. Each sweep call is bounded by `min(sweep deadline, now +
    // WRITE_BOUND)`.

    #[test]
    fn the_history_timeout_is_within_the_write_bound() {
        assert!(HISTORY_TIMEOUT <= WRITE_BOUND);
    }

    /// The `kausa_history_writes_total` lines of `/metrics`.
    fn write_counts(metrics: &Metrics) -> Vec<String> {
        metrics
            .render()
            .lines()
            .filter(|l| l.starts_with("kausa_history_writes_total"))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_closed_term_writes_no_explanation_and_counts_nothing() {
        let (url, requests) = api_server(vec![("200 OK", page(vec![], ""))]);
        let metrics = Metrics::default();
        block_on(async {
            history(&url)
                .write(&incident(), &explanation(), "m", &metrics, &Term::closed())
                .await
                .expect("a log");
        });
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "the API server was called"
        );
        assert_eq!(write_counts(&metrics), ["kausa_history_writes_total 0"]);
    }

    /// A fake API server that answers its first request after `wait`.
    fn slow_api_server(
        wait: Duration,
        status: &'static str,
        body: Value,
    ) -> (String, mpsc::Receiver<Sent>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            let mut open = Vec::new();
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = vec![0; 65536];
                let n = stream.read(&mut buffer).unwrap_or(0);
                let text = String::from_utf8_lossy(&buffer[..n]).into_owned();
                let line = text.lines().next().unwrap_or_default().to_owned();
                let _ = sent.send(Sent {
                    line,
                    head: String::new(),
                    body: String::new(),
                });
                if open.is_empty() {
                    std::thread::sleep(wait);
                    let body = body.to_string();
                    let answer = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: \
                         {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(answer.as_bytes());
                }
                open.push(stream);
            }
        });
        (url, received)
    }

    #[test]
    fn a_term_that_ends_during_the_list_makes_no_create() {
        let (url, requests) =
            slow_api_server(Duration::from_millis(300), "200 OK", page(vec![], ""));
        let metrics = Metrics::default();
        let term = Term::closed();
        block_on(async {
            term.extend_to(std::time::Instant::now() + Duration::from_millis(100));
            history(&url)
                .write(&incident(), &explanation(), "m", &metrics, &term)
                .await
                .expect("a log");
        });
        let first = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("the list");
        assert!(first.line.starts_with(LIST), "{}", first.line);
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "a call was made after the term ended"
        );
        assert_eq!(write_counts(&metrics), ["kausa_history_writes_total 0"]);
    }

    #[test]
    fn a_closed_term_sweeps_nothing() {
        let (url, requests) = api_server(vec![(
            "200 OK",
            page(vec![old("shop", "0123456789abcdef", "10")], ""),
        )]);
        let metrics = Metrics::default();
        block_on(async {
            let config = kube::Config::new(url.parse().expect("a URL"));
            History::new(Client::try_from(config).expect("a client"), 7)
                .sweep(&[], &metrics, &Term::closed())
                .await
                .expect("a log");
        });
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "the API server was called"
        );
        assert_eq!(counter(&metrics, "kausa_history_deleted_total"), 0);
        assert_eq!(counter(&metrics, "kausa_history_sweep_errors_total"), 0);
    }

    #[test]
    fn a_term_that_ends_during_a_sweep_list_makes_no_delete() {
        // The page lists an old object, but it arrives after the term has ended.
        let (url, requests) = slow_api_server(
            Duration::from_millis(300),
            "200 OK",
            page(vec![old("shop", "0123456789abcdef", "10")], ""),
        );
        let metrics = Metrics::default();
        let term = Term::closed();
        block_on(async {
            term.extend_to(std::time::Instant::now() + Duration::from_millis(100));
            let config = kube::Config::new(url.parse().expect("a URL"));
            History::new(Client::try_from(config).expect("a client"), 7)
                .sweep(&[], &metrics, &term)
                .await
                .expect("a log");
        });
        let first = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("the list");
        assert!(
            first
                .line
                .starts_with(&format!("GET {PLURAL}/explanations?")),
            "{}",
            first.line
        );
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "a delete was sent after the term ended"
        );
        assert_eq!(
            counter(&metrics, "kausa_history_deleted_total{reason=\"age\"}"),
            0
        );
        assert_eq!(counter(&metrics, "kausa_history_sweep_errors_total"), 0);
    }

    #[test]
    fn each_sweep_call_ends_within_the_write_bound() {
        // A list that is never answered ends at WRITE_BOUND, before the sweep's own SWEEP_TIME.
        assert!(WRITE_BOUND < SWEEP_TIME);
        let (url, _requests) =
            slow_api_server(Duration::from_secs(120), "200 OK", page(vec![], ""));
        let metrics = Metrics::default();
        let started = std::time::Instant::now();
        block_on(async {
            let config = kube::Config::new(url.parse().expect("a URL"));
            let sweep = History::new(Client::try_from(config).expect("a client"), 7);
            tokio::time::timeout(SWEEP_TIME, sweep.sweep(&[], &metrics, &Term::unbounded()))
                .await
                .expect("the sweep ends before SWEEP_TIME")
                .expect("a log");
        });
        let elapsed = started.elapsed();
        assert!(
            elapsed >= WRITE_BOUND && elapsed < WRITE_BOUND + Duration::from_secs(2),
            "{elapsed:?}"
        );
    }
}
