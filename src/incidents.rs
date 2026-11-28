//! The Incidents kausa creates and sweeps
//! (docs/adr/0028-incident-resource-and-remediation-report.md, points 1 and 4): the create in the
//! fact slot, after the incident's `Detected` Event, and the sweep, in a slot of its own. Not
//! `incident.rs`, which groups signals into incidents, nor `incident_resource.rs`, the pure part:
//! the `spec`, its check and the table of created Incidents.
//!
//! Only with `--incidents`, which sets `Options.incidents`; its counts are the
//! `kausa_incident*` counters of `metrics.rs`, which only then have a series.

use std::cell::RefCell;
use std::io;
use std::time::{Duration, Instant};

use futures::AsyncReadExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::jiff::Timestamp;
use kube::api::{
    ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, PostParams,
    Preconditions,
};
use kube::{Api, Client};
use serde_json::{Value, json};

use crate::history::pure::{Reason, SweepPlan};
use crate::history::{SWEEP_PAGE, SWEEP_TIME, api_failure, times};
use crate::incident_resource::table::{Admission, Entry, Limiter, Refusal, Table};
use crate::incident_resource::{Draft, Uid};
use crate::leader::Term;
use crate::leader::pure::WRITE_BOUND;
use crate::log;
use crate::metrics::Metrics;
use crate::owner::{OwnerLookup, is_dns_label};
use crate::report::Fact;

/// What `--incidents` sets: the window of the gate, `--dedup-hours`, and the days after which the
/// sweep deletes an Incident by its `creationTimestamp`, `--incident-days` (1 to 30, default 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub window: Duration,
    pub days: u32,
}

/// The counters of `metrics::INCIDENT_COUNTERS`, in its order.
pub(crate) const REFUSED: &str = "kausa_incidents_refused_total";
pub(crate) const WRITE_ERRORS: &str = "kausa_incident_write_errors_total";
pub(crate) const DELETED: &str = "kausa_incidents_deleted_total";
pub(crate) const SWEEP_ERRORS: &str = "kausa_incident_sweep_errors_total";

/// The most bytes of the create's answer kausa reads: the object the server stored, whose `spec`
/// the schema bounds (about 3 KiB at most) and whose `metadata` the server fills. An answer that
/// does not fit is a write error.
pub(crate) const CREATE_ANSWER_BYTES: u64 = 64 * 1024;

/// The path of a schema refusal for a namespace that is not a DNS label, which the server never
/// gives an object: `Field` names only the fields of the `spec` and `metadata.name`.
const NAMESPACE_FIELD: &str = "metadata.namespace";

/// The error of a `list` answered `404`.
const CRD_MISSING: &str =
    "the CustomResourceDefinition incidents.kausa.fedeam.com is not installed";

/// How a sweep ended. A sweep that completed, failed or stopped at its bound lets creates through;
/// one cut by the loss of the term does not, and the next term's sweep counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    Completed,
    /// A list or a delete the server refused, the sweep going on after it.
    Failed,
    /// At `SWEEP_TIME`, or at a call that did not end within `WRITE_BOUND`.
    Stopped,
    /// The term no longer held before a call.
    Cut,
}

impl Ended {
    /// Whether the gate's flag is set by this ending.
    pub(crate) fn ended(self) -> bool {
        self != Ended::Cut
    }
}

/// The resource, written out: kube's `derive` feature stays off (docs/adr/0014).
fn resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("kausa.fedeam.com", "v1alpha1", "Incident"),
        "incidents",
    )
}

/// The Incidents of one process: its client, the table of the Incidents it created, the hour
/// marks of the `incident refused` line and the metrics it counts in. None of it is ever borrowed
/// across an `await`.
pub struct Incidents<'a> {
    client: Client,
    resource: ApiResource,
    window: Duration,
    days: u32,
    table: RefCell<Table>,
    limiter: RefCell<Limiter>,
    metrics: &'a Metrics,
}

impl<'a> Incidents<'a> {
    pub fn new(client: Client, settings: Settings, metrics: &'a Metrics) -> Incidents<'a> {
        Incidents {
            client,
            resource: resource(),
            window: settings.window,
            days: settings.days,
            table: RefCell::new(Table::default()),
            limiter: RefCell::new(Limiter::default()),
            metrics,
        }
    }

    /// The Incident of `fact`, after its `Detected` Event and its `detected` message, in this
    /// order: the gate (a duplicate inside the window stops here, silently), the flag of the sweep
    /// (`starting`), room in the table (`table-full`), the check of what the `spec` holds before
    /// the read (`schema`), the read of the revision of a sample Pod run by a ReplicaSet, through
    /// the term's `lookup`, the final check, the create. A refusal sends nothing, and a failed
    /// create is not tried again; neither marks the fingerprint. The read and the create are made
    /// only while `term` holds. Only an unwritable log is returned.
    pub async fn create(
        &self,
        fact: &Fact,
        lookup: &impl OwnerLookup,
        term: &Term,
    ) -> io::Result<()> {
        let Some(data) = fact.incident.as_deref() else {
            return Ok(());
        };
        let namespace = fact.namespace.as_str();
        let admission =
            self.table
                .borrow()
                .admit(namespace, &fact.fingerprint, self.window, Instant::now());
        match admission {
            Admission::Duplicate => return Ok(()),
            Admission::Refused(reason) => return self.refuse(reason, fact, None),
            Admission::Create => {}
        }
        if !is_dns_label(namespace) {
            return self.refuse(Refusal::Schema, fact, Some(NAMESPACE_FIELD));
        }
        let draft = match Draft::of(fact, data) {
            Ok(draft) => draft,
            Err(field) => return self.refuse(Refusal::Schema, fact, Some(field.as_str())),
        };
        if let Err(field) = draft.spec.check() {
            return self.refuse(Refusal::Schema, fact, Some(field.as_str()));
        }
        let read = match &draft.read {
            Some(_) if !term.holds() => {
                return self.write_failed(fact, "the term ended before the read".to_owned());
            }
            Some(read) => lookup.replica_set(namespace, &read.name).await.ok(),
            None => None,
        };
        let spec = draft.after_read(read);
        let checked = match spec.checked() {
            Ok(checked) => checked,
            Err(field) => return self.refuse(Refusal::Schema, fact, Some(field.as_str())),
        };
        if !term.holds() {
            return self.write_failed(fact, "the term ended before the create".to_owned());
        }
        let uid = match self.send(namespace, &checked.name, checked.spec).await {
            Ok(uid) => uid,
            Err(error) => return self.write_failed(fact, error),
        };
        // The entry's time is the answer's arrival, never the request's (point 1).
        let created = Instant::now();
        let inserted = Entry::new(namespace, &spec, uid, created)
            .map(|entry| self.table.borrow_mut().insert(entry, created));
        match inserted {
            Some(Ok(_)) => Ok(()),
            // Not reached: the check passed, and the table had room at the gate, which nothing
            // but this slot fills.
            Some(Err(_)) => self.write_failed(fact, "the table is full".to_owned()),
            None => self.write_failed(fact, "the entry was refused".to_owned()),
        }
    }

    /// One create of the Incident `name` in `namespace`, bounded by `WRITE_BOUND` and its answer
    /// by `CREATE_ANSWER_BYTES`: the UID the server gave it, or what the log says of the failure.
    /// An error status is read by kube-rs, whole, as for every request kausa makes; only its code
    /// and reason are kept.
    async fn send(&self, namespace: &str, name: &str, spec: Value) -> Result<Uid, String> {
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), namespace, &self.resource);
        let request = kube::core::Request::new(api.resource_url())
            .create(&PostParams::default(), body(namespace, name, spec))
            .map_err(|error| error.to_string())?;
        let answer = tokio::time::timeout(WRITE_BOUND, async {
            let reader = self
                .client
                .request_stream(request)
                .await
                .map_err(|error| api_failure(&error))?;
            let reader = std::pin::pin!(reader);
            let mut bytes = Vec::new();
            reader
                .take(CREATE_ANSWER_BYTES + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| format!("the answer could not be read: {error}"))?;
            Ok::<_, String>(bytes)
        })
        .await
        .map_err(|_| format!("no answer within {} seconds", WRITE_BOUND.as_secs()))??;
        if answer.len() as u64 > CREATE_ANSWER_BYTES {
            return Err(format!("the answer is over {CREATE_ANSWER_BYTES} bytes"));
        }
        serde_json::from_slice::<Value>(&answer)
            .ok()
            .and_then(|object| {
                object
                    .pointer("/metadata/uid")
                    .and_then(Value::as_str)
                    .and_then(Uid::parse)
            })
            .ok_or_else(|| "the answer carries no UID".to_owned())
    }

    /// Counts a refusal and logs it, at most once an hour for its reason, naming its field (for
    /// `schema`) and its namespace, never a value.
    fn refuse(&self, reason: Refusal, fact: &Fact, field: Option<&str>) -> io::Result<()> {
        self.metrics.inc(REFUSED, &[("reason", reason.as_str())]);
        if self.limiter.borrow_mut().allow(reason, Instant::now()) {
            let namespace = is_dns_label(&fact.namespace).then_some(fact.namespace.as_str());
            log::warn(
                "incident refused",
                json!({
                    "reason": reason.as_str(),
                    "namespace": namespace,
                    "fingerprint": fact.fingerprint,
                    "field": field,
                }),
            )?;
        }
        Ok(())
    }

    fn write_failed(&self, fact: &Fact, error: String) -> io::Result<()> {
        self.metrics.inc(WRITE_ERRORS, &[]);
        log::warn(
            "incident write failed",
            json!({"fingerprint": fact.fingerprint, "namespace": fact.namespace, "error": error}),
        )
    }

    /// One sweep, 0014's applied to Incidents: deletes those created more than `days` days ago and,
    /// oldest first, those beyond `MAX_OBJECTS`, by `creationTimestamp` only. It lists every
    /// Incident, with no label selector, in pages of `SWEEP_PAGE`, across the cluster when
    /// `namespaces` is empty or in each listed namespace in order, keeps only five fields of each
    /// one's `metadata` before it reduces it, deletes as each page arrives, each delete with the
    /// UID and the `resourceVersion` listed as preconditions, and stops at `SWEEP_TIME`. A failed
    /// list ends its namespace and the sweep goes on with the next; a delete answered `404` or
    /// `409` is neither an error nor a deletion. Each call is made only while `term` holds and ends
    /// within `WRITE_BOUND`, or at the deadline if that comes first; a call that does not end in
    /// time ends the sweep. Unless the term ended, the gate's flag is set when it ends.
    pub async fn sweep(&self, namespaces: &[String], term: &Term) -> io::Result<Ended> {
        let ended = self.sweep_calls(namespaces, term).await?;
        if ended.ended() {
            self.table.borrow_mut().start_sweep_ended();
        }
        Ok(ended)
    }

    async fn sweep_calls(&self, namespaces: &[String], term: &Term) -> io::Result<Ended> {
        let deadline = tokio::time::Instant::now() + SWEEP_TIME;
        let call_end = || deadline.min(tokio::time::Instant::now() + WRITE_BOUND);
        let (_, cutoff) = times(Timestamp::now(), self.days);
        let mut plan = SweepPlan::incidents(&cutoff);
        let (mut by_age, mut by_cap) = (0_u64, 0_u64);
        let mut ended = Ended::Completed;
        let scopes: Vec<Option<&str>> = if namespaces.is_empty() {
            vec![None]
        } else {
            namespaces.iter().map(|n| Some(n.as_str())).collect()
        };
        'scopes: for scope in scopes {
            // The log names the namespace of the flag (null in cluster scope), never a listed one.
            let failed = |error: String| {
                self.metrics.inc(SWEEP_ERRORS, &[]);
                log::warn(
                    "incident sweep failed",
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
                let mut params = ListParams::default().limit(SWEEP_PAGE);
                if let Some(token) = &token {
                    params = params.continue_token(token);
                }
                if !term.holds() {
                    ended = Ended::Cut;
                    break 'scopes;
                }
                let Ok(listed) = tokio::time::timeout_at(call_end(), api.list(&params)).await
                else {
                    ended = Ended::Stopped;
                    break 'scopes;
                };
                let list = match listed {
                    Ok(list) => list,
                    Err(error) => {
                        failed(list_failure(&error))?;
                        ended = Ended::Failed;
                        continue 'scopes;
                    }
                };
                token = list.metadata.continue_.filter(|token| !token.is_empty());
                // Each object is reduced at once (invariant 8) and is untrusted (invariant 5).
                let items: Vec<Value> = list.items.into_iter().map(read_metadata).collect();
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
                            uid: delete.uid.clone(),
                        }),
                        ..DeleteParams::default()
                    };
                    if !term.holds() {
                        ended = Ended::Cut;
                        break 'scopes;
                    }
                    let Ok(answer) =
                        tokio::time::timeout_at(call_end(), api.delete(&delete.name, &params))
                            .await
                    else {
                        ended = Ended::Stopped;
                        break 'scopes;
                    };
                    match answer {
                        Ok(_) => {
                            self.metrics
                                .inc(DELETED, &[("reason", delete.reason.label())]);
                            match delete.reason {
                                Reason::Age => by_age += 1,
                                Reason::Cap => by_cap += 1,
                            }
                        }
                        Err(kube::Error::Api(status)) if matches!(status.code, 404 | 409) => {}
                        Err(error) => {
                            failed(api_failure(&error))?;
                            ended = Ended::Failed;
                        }
                    }
                }
                if token.is_none() {
                    break;
                }
            }
        }
        if by_age + by_cap > 0 {
            log::info(
                "incidents swept",
                json!({"deleted_by_age": by_age, "deleted_by_cap": by_cap}),
            )?;
        }
        Ok(ended)
    }

    #[cfg(test)]
    pub(crate) fn refused(&self, reason: Refusal) -> u64 {
        self.metrics
            .counter(REFUSED, &[("reason", reason.as_str())])
    }

    #[cfg(test)]
    pub(crate) fn write_errors(&self) -> u64 {
        self.metrics.counter(WRITE_ERRORS, &[])
    }

    #[cfg(test)]
    pub(crate) fn deleted(&self, reason: Reason) -> u64 {
        self.metrics.counter(DELETED, &[("reason", reason.label())])
    }

    #[cfg(test)]
    pub(crate) fn sweep_errors(&self) -> u64 {
        self.metrics.counter(SWEEP_ERRORS, &[])
    }

    #[cfg(test)]
    pub(crate) fn table(&self) -> std::cell::Ref<'_, Table> {
        self.table.borrow()
    }

    #[cfg(test)]
    pub(crate) fn table_mut(&self) -> std::cell::RefMut<'_, Table> {
        self.table.borrow_mut()
    }
}

/// The body of the create: the Incident `name` in `namespace` with `spec`, and no other field: no
/// label, no annotation, no `status`.
fn body(namespace: &str, name: &str, spec: Value) -> Vec<u8> {
    json!({
        "apiVersion": "kausa.fedeam.com/v1alpha1",
        "kind": "Incident",
        "metadata": {"name": name, "namespace": namespace},
        "spec": spec,
    })
    .to_string()
    .into_bytes()
}

/// A listed Incident as the sweep reads it: `metadata` with its namespace, name, UID,
/// `resourceVersion` and `creationTimestamp`, and nothing else, so neither its `spec` nor any other
/// metadata reaches the reducer.
fn read_metadata(object: DynamicObject) -> Value {
    let metadata = object.metadata;
    let kept = ObjectMeta {
        namespace: metadata.namespace,
        name: metadata.name,
        uid: metadata.uid,
        resource_version: metadata.resource_version,
        creation_timestamp: metadata.creation_timestamp,
        ..ObjectMeta::default()
    };
    json!({"metadata": serde_json::to_value(kept).unwrap_or(Value::Null)})
}

/// What the log says about a failed `list`: a `404` means the CRD is not installed; anything else
/// is told as `api_failure` tells it.
fn list_failure(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) if status.code == 404 => CRD_MISSING.to_owned(),
        _ => api_failure(error),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::incident_resource::tests::{
        UID_RS, conforms, data, event_fact, rollout_fact, sample_fact, schema, stateful_fact,
    };
    use crate::owner::Hop;
    use crate::record::OwnerRef;
    use crate::report::About;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const UID_CREATED: &str = "99999999-8888-4777-8666-555555555555";
    /// The create's path; kube-rs ends it with an empty query, as for every create it sends.
    pub(crate) const PATH: &str = "/apis/kausa.fedeam.com/v1alpha1/namespaces/shop/incidents?";

    /// A request the fake API server received.
    #[derive(Clone, Debug)]
    pub(crate) struct Sent {
        pub(crate) method: String,
        /// The path and the query.
        pub(crate) path: String,
        pub(crate) body: String,
    }

    impl Sent {
        pub(crate) fn json(&self) -> Value {
            serde_json::from_str(&self.body).expect("a JSON body")
        }
    }

    /// How the fake API server answers a request.
    pub(crate) enum Answer {
        /// A status code and a body.
        Reply(u16, String),
        /// No answer: the connection stays open.
        Hang,
    }

    pub(crate) fn reply(code: u16, body: Value) -> Answer {
        Answer::Reply(code, body.to_string())
    }

    pub(crate) fn failure(code: u16) -> Answer {
        reply(
            code,
            json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {},
                "status": "Failure",
                "message": "an invented message",
                "reason": "an invented reason",
                "code": code,
            }),
        )
    }

    pub(crate) type Requests = Arc<Mutex<Vec<Sent>>>;

    /// A fake API server: one connection per request, each answered by `answer`, in the order
    /// they arrive; every request is kept, in that order.
    pub(crate) fn api_server(
        answer: impl Fn(&Sent) -> Answer + Send + 'static,
    ) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let requests = Requests::default();
        let kept = Arc::clone(&requests);
        std::thread::spawn(move || {
            let mut open = Vec::new();
            while let Ok((mut stream, _)) = listener.accept() {
                let Some(sent) = read_request(&mut stream) else {
                    continue;
                };
                let answer = answer(&sent);
                kept.lock().expect("the requests").push(sent);
                match answer {
                    Answer::Reply(code, body) => {
                        let answer = format!(
                            "HTTP/1.1 {code} Fake\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(answer.as_bytes());
                    }
                    Answer::Hang => open.push(stream),
                }
            }
        });
        (url, requests)
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Option<Sent> {
        let mut request = Vec::new();
        let mut buffer = vec![0; 65536];
        loop {
            let n = stream.read(&mut buffer).ok()?;
            request.extend_from_slice(&buffer[..n]);
            let text = String::from_utf8_lossy(&request).into_owned();
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
                    let line = text.lines().next().unwrap_or_default();
                    let mut parts = line.split(' ');
                    return Some(Sent {
                        method: parts.next().unwrap_or_default().to_owned(),
                        path: parts.next().unwrap_or_default().to_owned(),
                        body: text[end + 4..].to_owned(),
                    });
                }
            }
            if n == 0 {
                return None;
            }
        }
    }

    pub(crate) fn sent(requests: &Requests) -> Vec<Sent> {
        requests.lock().expect("the requests").clone()
    }

    /// A runtime for one test: a client is used only on the runtime it was built on.
    pub(crate) fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
    }

    pub(crate) fn client(url: &str) -> Client {
        Client::try_from(kube::Config::new(url.parse().expect("a URL"))).expect("a client")
    }

    pub(crate) const SETTINGS: Settings = Settings {
        window: Duration::from_secs(24 * 3600),
        days: 1,
    };

    /// Metrics of their own for one test's Incidents, which live as long as the test process.
    pub(crate) fn metrics() -> &'static Metrics {
        Box::leak(Box::default())
    }

    /// The Incidents of the fake server at `url`, past their first sweep.
    pub(crate) fn started(url: &str) -> Incidents<'static> {
        let incidents = Incidents::new(client(url), SETTINGS, metrics());
        incidents.table_mut().start_sweep_ended();
        incidents
    }

    pub(crate) fn created() -> Answer {
        reply(201, json!({"metadata": {"uid": UID_CREATED}}))
    }

    /// A lookup that answers each read of a ReplicaSet with `answer` and keeps the names read.
    pub(crate) struct Revisions {
        pub(crate) answer: Result<(Uid, u64), ()>,
        pub(crate) read: RefCell<Vec<String>>,
    }

    impl Revisions {
        pub(crate) fn of(uid: &str, revision: u64) -> Self {
            Revisions {
                answer: Ok((Uid::parse(uid).expect("a UID"), revision)),
                read: RefCell::new(Vec::new()),
            }
        }
    }

    impl OwnerLookup for Revisions {
        async fn controller_of(
            &self,
            _hop: Hop,
            _namespace: &str,
            _name: &str,
        ) -> Result<Option<OwnerRef>, String> {
            panic!("the create reads no owner")
        }

        async fn replica_set(&self, namespace: &str, name: &str) -> Result<(Uid, u64), ()> {
            self.read.borrow_mut().push(format!("{namespace}/{name}"));
            self.answer
        }
    }

    fn create(
        rt: &tokio::runtime::Runtime,
        incidents: &Incidents,
        fact: &Fact,
        lookup: &Revisions,
        term: &Term,
    ) {
        rt.block_on(incidents.create(fact, lookup, term))
            .expect("a log");
    }

    /// The four golden bodies: what the create of each kind of fact sends, byte for byte, with
    /// the newline the file ends with.
    const GOLDEN: [(&str, &str); 4] = [
        (
            "replica-set",
            include_str!("../tests/golden/incident-create-replica-set.json"),
        ),
        (
            "no-replica-set",
            include_str!("../tests/golden/incident-create-no-replica-set.json"),
        ),
        (
            "regarding",
            include_str!("../tests/golden/incident-create-regarding.json"),
        ),
        (
            "deployment",
            include_str!("../tests/golden/incident-create-deployment.json"),
        ),
    ];

    fn golden_fact(name: &str) -> Fact {
        match name {
            "replica-set" => sample_fact(),
            "no-replica-set" => stateful_fact(),
            "regarding" => event_fact(),
            _ => rollout_fact(),
        }
    }

    #[test]
    fn each_create_sends_its_golden_body_and_keeps_the_answer_s_uid() {
        for (name, golden) in GOLDEN {
            let (url, requests) = api_server(|_| created());
            let lookup = Revisions::of(UID_RS, 7);
            let rt = runtime();
            let incidents = rt.block_on(async { started(&url) });
            let fact = golden_fact(name);
            create(&rt, &incidents, &fact, &lookup, &Term::unbounded());
            let sent = sent(&requests);
            assert_eq!(sent.len(), 1, "{name}");
            assert_eq!(
                (sent[0].method.as_str(), sent[0].path.as_str()),
                ("POST", PATH)
            );
            let expected = golden.strip_suffix('\n').expect("a final newline");
            assert_eq!(
                sent[0].body, expected,
                "{name}: the body differs from tests/golden"
            );
            let body = sent[0].json();
            conforms(
                &json!({"spec": body["spec"], "status": {}}),
                &schema(),
                name,
            );
            // Only a sample run by a ReplicaSet is read, by the name its Pod's reference gives.
            let read = lookup.read.borrow().clone();
            if name == "replica-set" {
                assert_eq!(read, ["shop/web-7d4b9c6f5d"], "{name}");
            } else {
                assert!(read.is_empty(), "{name}: {read:?}");
            }
            let table = incidents.table();
            let uid = Uid::parse(UID_CREATED).expect("a UID");
            let name_ = body["metadata"]["name"].as_str().expect("a name");
            let (_, entry) = table.find("shop", name_, uid).expect("the entry");
            assert_eq!(entry.fingerprint(), fact.fingerprint);
            assert_eq!(table.len(), 1);
            assert_eq!(incidents.write_errors(), 0);
            assert!(Refusal::ALL.iter().all(|&r| incidents.refused(r) == 0));
        }
    }

    #[test]
    fn a_duplicate_inside_the_window_sends_nothing_and_is_not_a_refusal() {
        let (url, requests) = api_server(|_| created());
        let lookup = Revisions::of(UID_RS, 7);
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(&rt, &incidents, &sample_fact(), &lookup, &Term::unbounded());
        create(&rt, &incidents, &sample_fact(), &lookup, &Term::unbounded());
        assert_eq!(sent(&requests).len(), 1);
        assert_eq!(lookup.read.borrow().len(), 1);
        assert!(Refusal::ALL.iter().all(|&r| incidents.refused(r) == 0));
        // Another namespace is another incident.
        let mut other = sample_fact();
        other.namespace = "web".into();
        create(&rt, &incidents, &other, &lookup, &Term::unbounded());
        assert_eq!(sent(&requests).len(), 2);
    }

    /// Each refusal before the read: no read, no request, one count of its reason, nothing kept.
    fn refused(
        rt: &tokio::runtime::Runtime,
        incidents: &Incidents,
        fact: &Fact,
        reason: Refusal,
        requests: &Requests,
    ) {
        let lookup = Revisions::of(UID_RS, 7);
        let before = incidents.table().len();
        create(rt, incidents, fact, &lookup, &Term::unbounded());
        assert!(sent(requests).is_empty(), "{reason:?}");
        assert!(lookup.read.borrow().is_empty(), "{reason:?}");
        assert_eq!(incidents.refused(reason), 1, "{reason:?}");
        assert_eq!(incidents.table().len(), before, "{reason:?}");
    }

    #[test]
    fn a_refusal_sends_no_read_and_no_create() {
        // Before a sweep of this process ended.
        let (url, requests) = api_server(|_| created());
        let rt = runtime();
        let incidents = rt.block_on(async { Incidents::new(client(&url), SETTINGS, metrics()) });
        refused(
            &rt,
            &incidents,
            &sample_fact(),
            Refusal::Starting,
            &requests,
        );
        // A table of young entries of other fingerprints.
        let (url, requests) = api_server(|_| created());
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        {
            let spec = crate::incident_resource::tests::pod_spec();
            let uid = Uid::parse(UID_CREATED).expect("a UID");
            let mut table = incidents.table_mut();
            for i in 0..crate::incident_resource::table::CAPACITY {
                let spec = crate::incident_resource::Spec {
                    fingerprint: format!("{i:016x}"),
                    ..spec.clone()
                };
                let entry = Entry::new("other", &spec, uid, Instant::now()).expect("an entry");
                table.insert(entry, Instant::now()).expect("room");
            }
        }
        refused(
            &rt,
            &incidents,
            &sample_fact(),
            Refusal::TableFull,
            &requests,
        );
        // What the spec holds before the read does not fit the schema: a Pod with no UID, a
        // namespace that is not a DNS label, a Pod name over 253 characters.
        let mut no_uid = sample_fact();
        if let About::Sample { pod_uid, .. } = &mut data(&mut no_uid).about {
            *pod_uid = None;
        }
        let mut namespace = sample_fact();
        namespace.namespace = "Shop_".into();
        let mut long = sample_fact();
        long.pod = Some("a".repeat(254));
        for fact in [no_uid, namespace, long] {
            let (url, requests) = api_server(|_| created());
            let rt = runtime();
            let incidents = rt.block_on(async { started(&url) });
            refused(&rt, &incidents, &fact, Refusal::Schema, &requests);
        }
    }

    /// A failed create: counted once, nothing kept, and the fingerprint not marked, so the next
    /// fact of the same incident is created.
    fn write_error(answer: Answer, term: &Term, requests_sent: usize) {
        let answered = Mutex::new(Some(answer));
        let (url, requests) = api_server(move |_| {
            answered
                .lock()
                .expect("the answer")
                .take()
                .unwrap_or_else(created)
        });
        let lookup = Revisions::of(UID_RS, 7);
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(&rt, &incidents, &sample_fact(), &lookup, term);
        assert_eq!(sent(&requests).len(), requests_sent);
        assert_eq!(incidents.write_errors(), 1);
        assert_eq!(incidents.table().len(), 0);
        assert!(Refusal::ALL.iter().all(|&r| incidents.refused(r) == 0));
        create(&rt, &incidents, &sample_fact(), &lookup, &Term::unbounded());
        assert_eq!(sent(&requests).len(), requests_sent + 1);
        assert_eq!(incidents.write_errors(), 1);
        assert_eq!(incidents.table().len(), 1);
    }

    /// A refusal whose body also carries the created UID where kausa reads it: only its status
    /// tells it from an accepted create.
    fn refused_with_a_uid(code: u16) -> Answer {
        reply(
            code,
            json!({
                "apiVersion": "v1",
                "kind": "Status",
                "metadata": {"uid": UID_CREATED},
                "status": "Failure",
                "message": "an invented message",
                "reason": "an invented reason",
                "code": code,
            }),
        )
    }

    #[test]
    fn a_failed_create_is_counted_keeps_nothing_and_marks_nothing() {
        let unbounded = Term::unbounded();
        // The same body, accepted, is a create: each refusal below fails by its status alone.
        let (url, _) = api_server(|_| refused_with_a_uid(201));
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(
            &rt,
            &incidents,
            &sample_fact(),
            &Revisions::of(UID_RS, 7),
            &unbounded,
        );
        assert_eq!(incidents.table().len(), 1);
        write_error(refused_with_a_uid(409), &unbounded, 1);
        write_error(refused_with_a_uid(500), &unbounded, 1);
        write_error(refused_with_a_uid(403), &unbounded, 1);
        write_error(failure(409), &unbounded, 1);
        // Accepted, with no UID, a UID that is not one, or an answer over the cap.
        write_error(reply(201, json!({"metadata": {}})), &unbounded, 1);
        write_error(
            reply(201, json!({"metadata": {"uid": "not-a-uid"}})),
            &unbounded,
            1,
        );
        write_error(Answer::Reply(201, "not JSON".to_owned()), &unbounded, 1);
        let padding = "a".repeat(CREATE_ANSWER_BYTES as usize);
        let over = json!({"metadata": {"uid": UID_CREATED}, "padding": padding});
        write_error(reply(201, over), &unbounded, 1);
        // A term that ended: no read and no request.
        write_error(created(), &Term::closed(), 0);
    }

    #[test]
    fn under_a_closed_term_a_fact_with_no_read_sends_nothing() {
        let (url, requests) = api_server(|_| created());
        let lookup = Revisions::of(UID_RS, 7);
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(&rt, &incidents, &stateful_fact(), &lookup, &Term::closed());
        assert!(sent(&requests).is_empty());
        assert!(lookup.read.borrow().is_empty());
        assert_eq!(incidents.write_errors(), 1);
        assert_eq!(incidents.table().len(), 0);
    }

    #[test]
    fn under_a_closed_term_a_fact_with_a_read_reads_nothing() {
        let (url, requests) = api_server(|_| created());
        let lookup = Revisions::of(UID_RS, 7);
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(&rt, &incidents, &sample_fact(), &lookup, &Term::closed());
        assert!(lookup.read.borrow().is_empty(), "the lookup was read");
        assert!(sent(&requests).is_empty());
        assert_eq!(incidents.write_errors(), 1);
    }

    #[test]
    fn the_entry_is_timed_by_the_answer_never_by_the_request() {
        // The server holds its answer well beyond the time it takes to send a request, and notes
        // when it answers: the entry is not older than that.
        let answered: Arc<Mutex<Option<Instant>>> = Arc::default();
        let noted = Arc::clone(&answered);
        let (url, requests) = api_server(move |_| {
            std::thread::sleep(Duration::from_millis(300));
            *noted.lock().expect("the time") = Some(Instant::now());
            created()
        });
        let lookup = Revisions::of(UID_RS, 7);
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(
            &rt,
            &incidents,
            &stateful_fact(),
            &lookup,
            &Term::unbounded(),
        );
        let sent = sent(&requests);
        let name = sent[0].json()["metadata"]["name"]
            .as_str()
            .expect("a name")
            .to_owned();
        let answered = answered.lock().expect("the time").expect("an answer");
        let table = incidents.table();
        let uid = Uid::parse(UID_CREATED).expect("a UID");
        let (_, entry) = table.find("shop", &name, uid).expect("the entry");
        assert!(
            entry.created() >= answered,
            "the entry is older than the answer"
        );
    }

    #[test]
    fn an_answer_at_the_cap_is_read() {
        let answer = |size: usize| {
            let mut body = json!({"metadata": {"uid": UID_CREATED}, "padding": ""});
            let pad = size - body.to_string().len();
            body["padding"] = json!("a".repeat(pad));
            assert_eq!(body.to_string().len(), size);
            body
        };
        let cap = CREATE_ANSWER_BYTES as usize;
        let (url, requests) = api_server(move |_| reply(201, answer(cap)));
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(
            &rt,
            &incidents,
            &sample_fact(),
            &Revisions::of(UID_RS, 7),
            &Term::unbounded(),
        );
        assert_eq!(sent(&requests).len(), 1);
        assert_eq!((incidents.write_errors(), incidents.table().len()), (0, 1));
        let (url, _) = api_server(move |_| reply(201, answer(cap + 1)));
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(
            &rt,
            &incidents,
            &sample_fact(),
            &Revisions::of(UID_RS, 7),
            &Term::unbounded(),
        );
        assert_eq!((incidents.write_errors(), incidents.table().len()), (1, 0));
    }

    #[test]
    fn a_create_with_no_answer_ends_at_the_write_bound() {
        let started_at = Instant::now();
        write_error(Answer::Hang, &Term::unbounded(), 1);
        let took = started_at.elapsed();
        assert!(took >= WRITE_BOUND, "{took:?}");
        assert!(took < WRITE_BOUND + Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn a_failed_read_is_written_as_revision_lookup_failed() {
        let (url, requests) = api_server(|_| created());
        let lookup = Revisions {
            answer: Err(()),
            read: RefCell::new(Vec::new()),
        };
        let rt = runtime();
        let incidents = rt.block_on(async { started(&url) });
        create(&rt, &incidents, &sample_fact(), &lookup, &Term::unbounded());
        let body = sent(&requests)[0].json();
        assert_eq!(body["spec"]["missing"], json!(["revision-lookup-failed"]));
        assert!(
            body["spec"]["sample"]["controller"]
                .get("revision")
                .is_none()
        );
        assert_eq!(incidents.table().len(), 1);
    }

    // --- The sweep (point 4) -------------------------------------------------------------------

    /// The list of every Incident across the cluster, before its query.
    const COLLECTION: &str = "/apis/kausa.fedeam.com/v1alpha1/incidents";

    /// The collection of the Incidents of `namespace`, before its query.
    fn collection(namespace: &str) -> String {
        format!("/apis/kausa.fedeam.com/v1alpha1/namespaces/{namespace}/incidents")
    }

    /// An Incident of `shop` the API server lists, created `ago` before now.
    fn listed(i: usize, ago: Duration) -> Value {
        let created = Timestamp::now() - ago;
        json!({
            "apiVersion": "kausa.fedeam.com/v1alpha1",
            "kind": "Incident",
            "metadata": {
                "name": format!("incident-{i:04}"),
                "namespace": "shop",
                "uid": format!("uid-{i}"),
                "resourceVersion": format!("rv-{i}"),
                "creationTimestamp": times(created, 0).0,
                "labels": {"example.com/any": "x"},
            },
            "spec": {"firstSeen": "1970-01-01T00:00:00Z"},
        })
    }

    const DAY: Duration = Duration::from_secs(86_400);

    /// The page of `objects` a list at `path` asks for, by its `limit` and its `continue` token,
    /// which is the index of the page's first object.
    fn page(objects: &[Value], path: &str) -> Answer {
        let (mut start, mut limit) = (0, objects.len());
        for pair in path.split_once('?').map_or("", |(_, q)| q).split('&') {
            match pair.split_once('=') {
                Some(("limit", v)) => limit = v.parse().expect("a limit"),
                Some(("continue", v)) => start = v.parse().expect("a token"),
                _ => {}
            }
        }
        let end = (start + limit).min(objects.len());
        let token = if end < objects.len() {
            end.to_string()
        } else {
            String::new()
        };
        reply(
            200,
            json!({
                "apiVersion": "kausa.fedeam.com/v1alpha1",
                "kind": "IncidentList",
                "metadata": {"continue": token, "resourceVersion": "1"},
                "items": objects[start..end],
            }),
        )
    }

    fn deleted_answer() -> Answer {
        reply(
            200,
            json!({"apiVersion": "v1", "kind": "Status", "metadata": {}, "status": "Success"}),
        )
    }

    /// The name of the Incident a delete at `path` is about: kube-rs ends the path with an empty
    /// query, as for a create.
    fn item(path: &str) -> &str {
        let path = path.split('?').next().unwrap_or_default();
        path.rsplit('/').next().unwrap_or_default()
    }

    /// The fake API server of a sweep: each list a page of `objects`, each delete `delete`'s
    /// answer for the name it deletes.
    fn sweep_server(
        objects: Vec<Value>,
        delete: impl Fn(&str) -> Answer + Send + 'static,
    ) -> (String, Requests) {
        api_server(move |request| match request.method.as_str() {
            "GET" => page(&objects, &request.path),
            "DELETE" => delete(item(&request.path)),
            _ => failure(405),
        })
    }

    /// A sweep of the Incidents at `url`, never started before: how it ended, and the Incidents.
    fn sweep(url: &str, namespaces: &[&str], term: &Term) -> (Ended, Incidents<'static>) {
        let rt = runtime();
        let namespaces: Vec<String> = namespaces.iter().map(|n| (*n).to_owned()).collect();
        rt.block_on(async {
            let incidents = Incidents::new(client(url), SETTINGS, metrics());
            let ended = incidents.sweep(&namespaces, term).await.expect("a log");
            (ended, incidents)
        })
    }

    #[test]
    fn a_sweep_of_a_thousand_incidents_deletes_the_oldest_five_hundred_with_both_preconditions() {
        // Created a minute apart, the newest a minute ago, all inside the day, listed shuffled.
        let objects: Vec<Value> = (0..1000)
            .map(|i| (i * 7919) % 1000)
            .map(|i| listed(i, Duration::from_secs(60 * (i as u64 + 1))))
            .collect();
        let (url, requests) = sweep_server(objects, |_| deleted_answer());
        let (ended, incidents) = sweep(&url, &[], &Term::unbounded());
        assert_eq!(ended, Ended::Completed);
        let requests = sent(&requests);
        let lists: Vec<&Sent> = requests.iter().filter(|r| r.method == "GET").collect();
        let deletes: Vec<&Sent> = requests.iter().filter(|r| r.method == "DELETE").collect();
        assert_eq!(lists.len(), 500, "pages of {SWEEP_PAGE}");
        assert_eq!(deletes.len(), 500);
        assert_eq!(requests.len(), 1000);
        for list in lists {
            let (path, query) = list.path.split_once('?').expect("a query");
            assert_eq!(path, COLLECTION);
            // No label selector: every Incident counts, whoever wrote it.
            assert!(query.split('&').any(|p| p == "limit=2"), "{query}");
            assert!(!query.contains("labelSelector"), "{query}");
        }
        let mut names = std::collections::BTreeSet::new();
        for delete in deletes {
            let path = delete.path.strip_suffix('?').expect("an empty query");
            let name = path.strip_prefix(&format!("{}/", collection("shop")));
            let name = name.expect("an Incident of shop").to_owned();
            let i: usize = name["incident-".len()..].parse().expect("an index");
            assert!(i >= 500, "{name} is not among the oldest");
            let preconditions = &delete.json()["preconditions"];
            assert_eq!(
                preconditions,
                &json!({"uid": format!("uid-{i}"), "resourceVersion": format!("rv-{i}")}),
                "{name}"
            );
            names.insert(name);
        }
        assert_eq!(names.len(), 500);
        assert_eq!(
            (
                incidents.deleted(Reason::Cap),
                incidents.deleted(Reason::Age)
            ),
            (500, 0)
        );
        assert_eq!(incidents.sweep_errors(), 0);
        assert!(incidents.table().sweep_ended());
    }

    #[test]
    fn a_delete_answered_404_or_409_is_neither_an_error_nor_a_deletion() {
        let objects: Vec<Value> = (0..3).map(|i| listed(i, 2 * DAY)).collect();
        let (url, requests) = sweep_server(objects, |name| match name {
            "incident-0000" => failure(404),
            "incident-0001" => failure(409),
            _ => deleted_answer(),
        });
        let (ended, incidents) = sweep(&url, &[], &Term::unbounded());
        assert_eq!(ended, Ended::Completed);
        let deletes = sent(&requests)
            .into_iter()
            .filter(|r| r.method == "DELETE")
            .count();
        assert_eq!(deletes, 3);
        assert_eq!(
            (
                incidents.deleted(Reason::Age),
                incidents.deleted(Reason::Cap)
            ),
            (1, 0)
        );
        assert_eq!(incidents.sweep_errors(), 0);
    }

    #[test]
    fn the_sweep_deletes_by_age_after_the_incident_days_not_after_one_day() {
        // With `--incident-days 3`: two days old is kept, four days old is deleted.
        let objects = vec![listed(0, 2 * DAY), listed(1, 4 * DAY)];
        let (url, requests) = sweep_server(objects, |_| deleted_answer());
        let rt = runtime();
        let settings = Settings {
            days: 3,
            ..SETTINGS
        };
        let (ended, incidents) = rt.block_on(async {
            let incidents = Incidents::new(client(&url), settings, metrics());
            let ended = incidents.sweep(&[], &Term::unbounded()).await;
            (ended.expect("a log"), incidents)
        });
        assert_eq!(ended, Ended::Completed);
        let deleted: Vec<String> = sent(&requests)
            .into_iter()
            .filter(|r| r.method == "DELETE")
            .map(|r| item(&r.path).to_owned())
            .collect();
        assert_eq!(deleted, ["incident-0001"]);
        assert_eq!(
            (
                incidents.deleted(Reason::Age),
                incidents.deleted(Reason::Cap)
            ),
            (1, 0)
        );
        assert_eq!(incidents.sweep_errors(), 0);
    }

    #[test]
    fn a_failed_sweep_goes_on_and_sets_the_flag() {
        // A list refused in one namespace ends that namespace only; a delete refused, that
        // delete only.
        let objects: Vec<Value> = (0..2).map(|i| listed(i, 2 * DAY)).collect();
        let (url, requests) = api_server(move |request| {
            if request.path.starts_with(&collection("a")) {
                failure(500)
            } else if request.method == "GET" {
                page(&objects, &request.path)
            } else if item(&request.path) == "incident-0000" {
                failure(403)
            } else {
                deleted_answer()
            }
        });
        let (ended, incidents) = sweep(&url, &["a", "b"], &Term::unbounded());
        assert_eq!(ended, Ended::Failed);
        let paths: Vec<String> = sent(&requests)
            .into_iter()
            .map(|r| {
                format!(
                    "{} {}",
                    r.method,
                    r.path.split('?').next().unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            paths,
            [
                format!("GET {}", collection("a")),
                format!("GET {}", collection("b")),
                format!("DELETE {}/incident-0000", collection("shop")),
                format!("DELETE {}/incident-0001", collection("shop")),
            ]
        );
        assert_eq!(
            (incidents.deleted(Reason::Age), incidents.sweep_errors()),
            (1, 2)
        );
        assert!(incidents.table().sweep_ended());
        // A list answered 404, the resource not installed, is a failure too.
        let (url, _) = api_server(|_| failure(404));
        let (ended, incidents) = sweep(&url, &[], &Term::unbounded());
        assert_eq!((ended, incidents.sweep_errors()), (Ended::Failed, 1));
        assert!(incidents.table().sweep_ended());
    }

    #[test]
    fn a_sweep_that_completes_or_is_cut_by_the_term_sets_the_flag_or_not() {
        let (url, requests) = sweep_server(Vec::new(), |_| deleted_answer());
        let (ended, incidents) = sweep(&url, &[], &Term::unbounded());
        assert_eq!(ended, Ended::Completed);
        assert!(incidents.table().sweep_ended());
        assert_eq!(sent(&requests).len(), 1);
        // A term that does not hold: no call, and the flag stays down.
        let (url, requests) = sweep_server(Vec::new(), |_| deleted_answer());
        let (ended, incidents) = sweep(&url, &[], &Term::closed());
        assert_eq!(ended, Ended::Cut);
        assert!(!incidents.table().sweep_ended());
        assert_eq!(sent(&requests).len(), 0);
        // Nor does a create get through after it: it is refused `starting`.
        let rt = runtime();
        let (url, requests) = api_server(|_| created());
        let incidents = rt.block_on(async { Incidents::new(client(&url), SETTINGS, metrics()) });
        let lookup = Revisions::of(UID_RS, 7);
        create(&rt, &incidents, &sample_fact(), &lookup, &Term::unbounded());
        assert_eq!(incidents.refused(Refusal::Starting), 1);
        assert_eq!(sent(&requests).len(), 0);
    }

    #[test]
    fn a_term_that_ends_between_a_list_and_its_deletes_sends_no_delete() {
        // Every listed Incident is older than the day; the list is answered after the term ended.
        let objects: Vec<Value> = (0..3).map(|i| listed(i, DAY * 2)).collect();
        let (held, hold) = crate::leader::hold::new();
        let held = Mutex::new(Some(held));
        let (url, requests) = api_server(move |request| match request.method.as_str() {
            "GET" => {
                if let Some(held) = held.lock().expect("the hold").take() {
                    held.wait();
                }
                page(&objects, &request.path)
            }
            _ => deleted_answer(),
        });
        let term = Term::unbounded();
        let (ended, incidents) = runtime().block_on(async {
            let incidents = Incidents::new(client(&url), SETTINGS, metrics());
            let (ended, ()) = futures::join!(incidents.sweep(&[], &term), hold.end(&term));
            (ended.expect("a log"), incidents)
        });
        assert_eq!(ended, Ended::Cut);
        let methods: Vec<String> = sent(&requests).into_iter().map(|r| r.method).collect();
        assert_eq!(methods, ["GET"]);
        assert!(!incidents.table().sweep_ended());
    }

    #[test]
    fn a_listed_incident_is_reduced_to_five_fields_of_its_metadata() {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "kausa.fedeam.com/v1alpha1",
            "kind": "Incident",
            "metadata": {
                "name": "incident-0001",
                "namespace": "shop",
                "uid": "uid-1",
                "resourceVersion": "rv-1",
                "creationTimestamp": "2026-01-02T03:04:05Z",
                "generation": 3,
                "labels": {"example.com/any": "x"},
                "annotations": {"example.com/note": "y"},
                "ownerReferences": [{
                    "apiVersion": "v1", "kind": "Pod", "name": "api-1", "uid": "uid-pod"
                }],
                "finalizers": ["example.com/keep"],
                "managedFields": [{"manager": "example", "operation": "Update"}],
            },
            "spec": {"firstSeen": "1970-01-01T00:00:00Z"},
            "status": {"phase": "any"},
        }))
        .expect("an object");
        assert_eq!(
            read_metadata(object),
            json!({"metadata": {
                "name": "incident-0001",
                "namespace": "shop",
                "uid": "uid-1",
                "resourceVersion": "rv-1",
                "creationTimestamp": "2026-01-02T03:04:05Z",
            }})
        );
    }

    #[test]
    fn a_list_with_no_answer_stops_the_sweep_at_the_write_bound_and_sets_the_flag() {
        let (url, requests) = api_server(|_| Answer::Hang);
        let start = Instant::now();
        let (ended, incidents) = sweep(&url, &[], &Term::unbounded());
        let took = start.elapsed();
        assert_eq!(ended, Ended::Stopped);
        assert!(
            took >= WRITE_BOUND && took < WRITE_BOUND + Duration::from_secs(2),
            "{took:?}"
        );
        assert_eq!(sent(&requests).len(), 1);
        assert!(incidents.table().sweep_ended());
    }

    #[test]
    fn a_sweep_stops_at_its_deadline() {
        // Each of the first three lists answers after 9.5 seconds, under the write bound; the
        // fourth never does, and the sweep's 30 seconds end it before its own 10 do.
        let objects: Vec<Value> = (0..8).map(|i| listed(i, Duration::from_secs(60))).collect();
        let lists = std::sync::atomic::AtomicUsize::new(0);
        let (url, requests) = api_server(move |request| {
            if lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 3 {
                return Answer::Hang;
            }
            std::thread::sleep(Duration::from_millis(9500));
            page(&objects, &request.path)
        });
        let start = Instant::now();
        let (ended, incidents) = sweep(&url, &[], &Term::unbounded());
        let took = start.elapsed();
        assert_eq!(ended, Ended::Stopped);
        assert!(
            took >= SWEEP_TIME && took < SWEEP_TIME + Duration::from_secs(2),
            "{took:?}"
        );
        assert_eq!(sent(&requests).len(), 4);
        assert!(incidents.table().sweep_ended());
    }

    /// The verbs of 0028's point 5 on `incidents`: nothing else is ever sent about one.
    const POINT_5: [&str; 3] = ["create", "delete", "list"];

    #[test]
    fn the_requests_about_incidents_are_only_point_5_s_create_list_and_delete() {
        let objects = vec![listed(0, 2 * DAY)];
        let (url, requests) = api_server(move |request| match request.method.as_str() {
            "GET" => page(&objects, &request.path),
            "DELETE" => deleted_answer(),
            _ => created(),
        });
        let rt = runtime();
        let incidents = rt.block_on(async { Incidents::new(client(&url), SETTINGS, metrics()) });
        for namespaces in [vec![], vec!["shop".to_owned()]] {
            let ended = rt.block_on(incidents.sweep(&namespaces, &Term::unbounded()));
            assert_eq!(ended.expect("a log"), Ended::Completed);
        }
        let lookup = Revisions::of(UID_RS, 7);
        create(&rt, &incidents, &sample_fact(), &lookup, &Term::unbounded());
        let mut verbs = std::collections::BTreeSet::new();
        for request in sent(&requests) {
            let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
            let verb = match (request.method.as_str(), path) {
                ("POST", p) if p == collection("shop") => "create",
                // A collection with a `limit` is a list; no `get` of one Incident is ever sent.
                ("GET", p)
                    if (p == COLLECTION || p == collection("shop")) && query.contains("limit=") =>
                {
                    "list"
                }
                ("DELETE", p) if p == format!("{}/incident-0000", collection("shop")) => "delete",
                _ => panic!(
                    "not a request of point 5: {} {}",
                    request.method, request.path
                ),
            };
            verbs.insert(verb);
        }
        assert_eq!(verbs.into_iter().collect::<Vec<_>>(), POINT_5);
    }
}
