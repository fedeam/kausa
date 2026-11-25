//! The Incidents kausa creates (docs/adr/0028-incident-resource-and-remediation-report.md,
//! point 1): the create in the fact slot, after the incident's `Detected` Event. Not
//! `incident.rs`, which groups signals into incidents, nor `incident_resource.rs`, the pure part:
//! the `spec`, its check and the table of created Incidents.
//!
//! Only with the internal option `Options.incidents`, which nothing sets yet: its counts are kept
//! here, readable by the tests, and are not rendered.

use std::cell::{Cell, RefCell};
use std::io;
use std::time::{Duration, Instant};

use futures::AsyncReadExt;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind, PostParams};
use kube::{Api, Client};
use serde_json::{Value, json};

use crate::history::api_failure;
use crate::incident_resource::table::{Admission, Entry, Limiter, Refusal, Table};
use crate::incident_resource::{Draft, Uid};
use crate::leader::Term;
use crate::leader::pure::WRITE_BOUND;
use crate::log;
use crate::owner::{OwnerLookup, is_dns_label};
use crate::report::Fact;

/// What turns incidents on, internal for now: the window of the gate, `--dedup-hours`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub window: Duration,
}

/// The most bytes of the create's answer kausa reads: the object the server stored, whose `spec`
/// the schema bounds (about 3 KiB at most) and whose `metadata` the server fills. An answer that
/// does not fit is a write error.
pub(crate) const CREATE_ANSWER_BYTES: u64 = 64 * 1024;

/// The path of a schema refusal for a namespace that is not a DNS label, which the server never
/// gives an object: `Field` names only the fields of the `spec` and `metadata.name`.
const NAMESPACE_FIELD: &str = "metadata.namespace";

/// The resource, written out: kube's `derive` feature stays off (docs/adr/0014).
fn resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("kausa.fedeam.com", "v1alpha1", "Incident"),
        "incidents",
    )
}

/// The Incidents of one process: its client, the table of the Incidents it created, the hour
/// marks of the `incident refused` line and the counts. None of it is ever borrowed across an
/// `await`.
pub struct Incidents {
    client: Client,
    resource: ApiResource,
    window: Duration,
    table: RefCell<Table>,
    limiter: RefCell<Limiter>,
    /// Creates refused, by `Refusal` (`kausa_incidents_refused_total{reason}` when rendered).
    refused: [Cell<u64>; Refusal::ALL.len()],
    /// Creates that failed (`kausa_incident_write_errors_total` when rendered).
    write_errors: Cell<u64>,
}

impl Incidents {
    pub fn new(client: Client, settings: Settings) -> Incidents {
        Incidents {
            client,
            resource: resource(),
            window: settings.window,
            table: RefCell::new(Table::default()),
            limiter: RefCell::new(Limiter::default()),
            refused: Default::default(),
            write_errors: Cell::new(0),
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
        let count = &self.refused[reason as usize];
        count.set(count.get() + 1);
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
        self.write_errors.set(self.write_errors.get() + 1);
        log::warn(
            "incident write failed",
            json!({"fingerprint": fact.fingerprint, "namespace": fact.namespace, "error": error}),
        )
    }

    #[cfg(test)]
    pub(crate) fn refused(&self, reason: Refusal) -> u64 {
        self.refused[reason as usize].get()
    }

    #[cfg(test)]
    pub(crate) fn write_errors(&self) -> u64 {
        self.write_errors.get()
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
    };

    /// The Incidents of the fake server at `url`, past their first sweep.
    pub(crate) fn started(url: &str) -> Incidents {
        let incidents = Incidents::new(client(url), SETTINGS);
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
        let incidents = rt.block_on(async { Incidents::new(client(&url), SETTINGS) });
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

    #[test]
    fn a_failed_create_is_counted_keeps_nothing_and_marks_nothing() {
        let unbounded = Term::unbounded();
        write_error(failure(409), &unbounded, 1);
        write_error(failure(500), &unbounded, 1);
        write_error(failure(403), &unbounded, 1);
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
}
