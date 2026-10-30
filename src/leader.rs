//! The leader election of the optional standby (docs/adr/0019-high-availability.md): one Lease
//! that the chart creates, read with `get` and written with a JSON merge patch of its `spec` under
//! a `resourceVersion` precondition. The decisions are in `pure.rs`; this file makes the calls,
//! keeps the time, and logs.
//!
//! Nothing read from the Lease is logged, sent or reused (invariant 5): only its version is echoed
//! as the next precondition and its holder compared, and every log line names the Lease by the
//! flag's own value.

pub(crate) mod pure;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::pin;
use std::time::{Duration, Instant};

use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::jiff::Timestamp;
use kube::Client;
use kube::api::{Api, Patch, PatchParams, ValidationDirective};
use serde_json::{Value, json};

use crate::log;
use crate::metrics::Metrics;
use pure::{AfterConflict, Standby, Timings};

/// How often a `lease error` that lasts is logged again (counted every time).
pub const LEASE_ERROR_PERIOD: Duration = Duration::from_secs(60);

/// The two calls kausa makes on its Lease, so that tests can script them. Nothing is spawned, so
/// the futures need not be `Send`.
pub(crate) trait LeaseApi {
    /// Reads the Lease.
    async fn get(&self) -> Result<Lease, LeaseError>;
    /// Sends a JSON merge patch and returns the Lease as written.
    async fn patch(&self, body: &Value) -> Result<Lease, LeaseError>;
}

/// A failed Lease call. Its text never carries the API server's message, which can quote the
/// stored Lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseError {
    /// `409`: the precondition failed (the Lease was written since it was read). Not an error.
    Conflict,
    /// `404`: the Lease does not exist. kausa never creates it.
    NotFound,
    /// `403`, with the code and reason of the answer.
    Forbidden(String),
    /// Any other failure, with the code and reason of the answer or the client's text.
    Other(String),
    /// No answer within the call's bound.
    Timeout,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => f.write_str("409 Conflict"),
            Self::NotFound => f.write_str("404 NotFound"),
            Self::Forbidden(text) | Self::Other(text) => f.write_str(text),
            Self::Timeout => f.write_str("no answer within the call's timeout"),
        }
    }
}

impl LeaseError {
    /// The error of a failed kube call. For an answer of the API server, only its code and reason
    /// are kept: its message can quote the stored Lease (as `history.rs` does for Explanations).
    /// A response that could not be decoded is not quoted either.
    fn from_kube(error: &kube::Error) -> Self {
        match error {
            kube::Error::Api(status) => {
                let text = format!(
                    "{} {} (the API server's message is not logged: it can quote the Lease)",
                    status.code, status.reason
                );
                match status.code {
                    409 => Self::Conflict,
                    404 => Self::NotFound,
                    403 => Self::Forbidden(text),
                    _ => Self::Other(text),
                }
            }
            kube::Error::SerdeError(_) => Self::Other("the answer could not be decoded".into()),
            _ => Self::Other(error.to_string()),
        }
    }
}

/// What to do about an error, logged beside it: on `403` and `404` only.
pub fn hint(error: &LeaseError) -> Option<&'static str> {
    match error {
        LeaseError::Forbidden(_) => {
            Some("kausa needs get and patch on this Lease (docs/reference/permissions.md)")
        }
        LeaseError::NotFound => Some("the chart creates this Lease: run helm upgrade"),
        _ => None,
    }
}

/// The Lease through the API server: `get`, and a merge patch with strict field validation, each
/// within `timeout`. The driver bounds every call too, so a scripted API is bounded the same way.
pub struct KubeLease {
    api: Api<Lease>,
    name: String,
    timeout: Duration,
}

impl KubeLease {
    /// The Lease `name` in `namespace`, each call bounded by `timeout` (the election's
    /// `call_timeout`).
    pub fn new(client: Client, namespace: &str, name: &str, timeout: Duration) -> Self {
        Self {
            api: Api::namespaced(client, namespace),
            name: name.to_owned(),
            timeout,
        }
    }
}

impl LeaseApi for KubeLease {
    async fn get(&self) -> Result<Lease, LeaseError> {
        bounded(self.timeout, async {
            self.api
                .get(&self.name)
                .await
                .map_err(|e| LeaseError::from_kube(&e))
        })
        .await
    }

    async fn patch(&self, body: &Value) -> Result<Lease, LeaseError> {
        let params = PatchParams {
            field_validation: Some(ValidationDirective::Strict),
            ..PatchParams::default()
        };
        bounded(self.timeout, async {
            self.api
                .patch(&self.name, &params, &Patch::Merge(body))
                .await
                .map_err(|e| LeaseError::from_kube(&e))
        })
        .await
    }
}

/// `call`, or `Timeout` once `limit` has passed (the call is dropped).
async fn bounded(
    limit: Duration,
    call: impl Future<Output = Result<Lease, LeaseError>>,
) -> Result<Lease, LeaseError> {
    tokio::time::timeout(limit, call)
        .await
        .unwrap_or(Err(LeaseError::Timeout))
}

/// The current term, read by the guard before every write: its end (`None` without
/// `--leader-lease`: the term always holds) and the Lease's version that `hold` last used, the
/// precondition of a release.
#[derive(Debug)]
pub struct Term {
    end: Cell<Option<Instant>>,
    version: RefCell<Option<String>>,
}

impl Term {
    /// A term that holds until it is closed: kausa without `--leader-lease`.
    pub fn unbounded() -> Self {
        Self {
            end: Cell::new(None),
            version: RefCell::new(None),
        }
    }

    /// A term that does not hold: a standby's, until it acquires the Lease.
    pub fn closed() -> Self {
        Self {
            end: Cell::new(Some(Instant::now())),
            version: RefCell::new(None),
        }
    }

    /// Whether this process may act now. A term ends on its own at its end, with no timer.
    pub fn holds(&self) -> bool {
        self.end.get().is_none_or(|end| Instant::now() < end)
    }

    /// Makes the term hold until `end`.
    pub fn extend_to(&self, end: Instant) {
        self.end.set(Some(end));
    }

    /// Ends the term now, an unbounded one too, and forgets its version.
    pub fn close(&self) {
        self.end.set(Some(Instant::now()));
        self.version.replace(None);
    }

    /// The version to release the Lease with: the last one `hold` used, while it held.
    pub fn version(&self) -> Option<String> {
        self.version.borrow().clone()
    }

    fn set_version(&self, version: &str) {
        self.version.replace(Some(version.to_owned()));
    }
}

/// The `lease error` log of one Lease: every error is counted in
/// `kausa_lease_errors_total{call}`, and logged when it starts, when its text changes, and once
/// per `period` while it lasts. A success of the same call ends it.
pub struct LeaseErrors {
    /// The flag's own `<namespace>/<name>`, the only name of the Lease that is logged.
    lease: String,
    period: Duration,
    /// Per call, the text last logged and when.
    logged: RefCell<BTreeMap<String, (String, Instant)>>,
}

impl LeaseErrors {
    pub fn new(lease: &str, period: Duration) -> Self {
        Self {
            lease: lease.to_owned(),
            period,
            logged: RefCell::default(),
        }
    }

    /// A call succeeded: an error of it that comes next is logged at once.
    pub fn ok(&self, call: &str) {
        self.logged.borrow_mut().remove(call);
    }

    /// Counts a failed `call` (`get` or `patch`) and logs it unless the same error was logged less
    /// than a period ago. Says whether it logged. A conflict is not an error: neither counted nor
    /// logged.
    pub fn report(&self, call: &str, error: &LeaseError, metrics: &Metrics) -> io::Result<bool> {
        if *error == LeaseError::Conflict {
            return Ok(false);
        }
        metrics.inc("kausa_lease_errors_total", &[("call", call)]);
        let text = error.to_string();
        let now = Instant::now();
        let log = {
            let mut logged = self.logged.borrow_mut();
            match logged.get(call) {
                Some((last, at)) if *last == text && now.duration_since(*at) < self.period => false,
                _ => {
                    logged.insert(call.to_owned(), (text.clone(), now));
                    true
                }
            }
        };
        if log {
            let mut fields = json!({"lease": self.lease, "call": call, "error": text});
            if let Some(hint) = hint(error) {
                fields["hint"] = json!(hint);
            }
            log::warn("lease error", fields)?;
        }
        Ok(log)
    }
}

/// The Lease, just acquired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acquired {
    /// The version the acquire returned: the precondition of the first renewal.
    pub version: String,
    /// When the acquire was sent: the term ends `renew_deadline` after it.
    pub sent: Instant,
}

/// Why a leader stopped holding the Lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lost {
    /// No renewal succeeded within `renew_deadline` of the last good one.
    NotRenewed,
    /// After a conflict, the Lease named another holder, or none.
    HeldByAnother,
}

/// Waits as a standby until this replica holds the Lease: reads it at once and then every
/// `retry_period`, and sends the acquire patch when the Lease is free, its own, or unchanged for
/// `lease_duration`. The first read that leaves it a standby (a wait, or a conflict on its acquire)
/// marks it Ready and logs `standby`, once per call. Errors are counted and logged through
/// `errors`; a missing Lease is never created. Returns only an unwritable log as an error.
pub async fn standby(
    api: &impl LeaseApi,
    me: &str,
    t: &Timings,
    metrics: &Metrics,
    errors: &LeaseErrors,
) -> io::Result<Acquired> {
    let mut announced = false;
    let mut waiting = || -> io::Result<()> {
        metrics.set_ready();
        if !announced {
            announced = true;
            log::info("standby", json!({"lease": errors.lease}))?;
        }
        Ok(())
    };
    let mut seen: Option<pure::Seen> = None;
    let mut reads = tokio::time::interval(t.retry_period);
    reads.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        reads.tick().await;
        let lease = match bounded(t.call_timeout, api.get()).await {
            Ok(lease) => {
                errors.ok("get");
                lease
            }
            Err(error) => {
                errors.report("get", &error, metrics)?;
                continue;
            }
        };
        let Some(observed) = pure::observe(&lease) else {
            errors.report("get", &no_version(), metrics)?;
            continue;
        };
        let now = Instant::now();
        let current = pure::seen(seen.take(), &observed, now);
        let decision = pure::standby(&observed, &current, me, now, t);
        seen = Some(current);
        if decision == Standby::Wait {
            waiting()?;
            continue;
        }
        let stored = lease.spec.as_ref().and_then(|spec| spec.lease_transitions);
        let transitions = pure::transitions(stored, observed.holder.as_deref(), me);
        let sent = Instant::now();
        let body = pure::acquire_body(&observed.version, me, Timestamp::now(), transitions);
        match bounded(t.call_timeout, api.patch(&body)).await {
            Ok(written) => {
                errors.ok("patch");
                match pure::observe(&written) {
                    Some(written) => {
                        return Ok(Acquired {
                            version: written.version,
                            sent,
                        });
                    }
                    // Without a version the next renewal has no precondition: read again (the
                    // Lease names this replica, so the next read takes it back).
                    None => {
                        errors.report("patch", &no_version(), metrics)?;
                    }
                }
            }
            // Another replica won this version.
            Err(LeaseError::Conflict) => waiting()?,
            Err(error) => {
                errors.report("patch", &error, metrics)?;
            }
        }
    }
}

/// A Lease answered without a `resourceVersion`, which kausa never patches without.
fn no_version() -> LeaseError {
    LeaseError::Other("the Lease came back without a resourceVersion".into())
}

/// Holds the Lease: extends `term` to `renew_deadline` after the acquire was sent, renews every
/// `retry_period` and extends it after each good renewal. A deadline timer is raced, first,
/// against the renewals, so a hung call never delays the step-down. The term keeps the version
/// last used, for a release. Returns when the term ends unrenewed or the Lease names another
/// holder; either way the term is closed. Returns only an unwritable log as an error.
pub async fn hold(
    api: &impl LeaseApi,
    me: &str,
    acquired: Acquired,
    term: &Term,
    t: &Timings,
    metrics: &Metrics,
    errors: &LeaseErrors,
) -> io::Result<Lost> {
    let mut version = acquired.version;
    let mut next = acquired.sent + t.retry_period;
    let end = pure::term_end(acquired.sent, t);
    term.extend_to(end);
    term.set_version(&version);
    let mut deadline = pin!(tokio::time::sleep_until(end.into()));
    loop {
        let (renewal, began) = tokio::select! {
            biased;
            () = &mut deadline => {
                term.close();
                return Ok(Lost::NotRenewed);
            }
            result = renew(api, me, &version, next, t, metrics, errors) => result?,
        };
        next = began + t.retry_period;
        match renewal {
            Renewal::Renewed {
                version: written,
                sent,
            } => {
                if let Some(written) = written {
                    version = written;
                    term.set_version(&version);
                }
                let end = pure::term_end(sent, t);
                term.extend_to(end);
                deadline.as_mut().reset(end.into());
            }
            Renewal::Failed { version: read } => {
                if let Some(read) = read {
                    version = read;
                    term.set_version(&version);
                }
            }
            Renewal::HeldByAnother => {
                term.close();
                return Ok(Lost::HeldByAnother);
            }
        }
    }
}

/// The outcome of one renewal attempt.
enum Renewal {
    /// A renewal sent at `sent` was written; `version` is what it returned (`None`: keep the last,
    /// and let the next renewal's conflict read it).
    Renewed {
        version: Option<String>,
        sent: Instant,
    },
    /// Not renewed: counted and logged, or a second conflict. `version` is a newer one read after a
    /// conflict.
    Failed { version: Option<String> },
    /// After a conflict, the Lease names another holder, or none.
    HeldByAnother,
}

/// One renewal attempt, begun at `at`: the renew patch with `version`; after a conflict, one read
/// and, while the Lease still names `me` (a Helm label change moved the version), one more renew
/// patch with the version read. Returns the outcome and when the attempt began.
async fn renew(
    api: &impl LeaseApi,
    me: &str,
    version: &str,
    at: Instant,
    t: &Timings,
    metrics: &Metrics,
    errors: &LeaseErrors,
) -> io::Result<(Renewal, Instant)> {
    tokio::time::sleep_until(at.into()).await;
    let began = Instant::now();
    let body = pure::renew_body(version, me, Timestamp::now());
    let renewal = match bounded(t.call_timeout, api.patch(&body)).await {
        Ok(written) => {
            errors.ok("patch");
            Renewal::Renewed {
                version: pure::observe(&written).map(|o| o.version),
                sent: began,
            }
        }
        Err(LeaseError::Conflict) => after_conflict(api, me, t, metrics, errors).await?,
        Err(error) => {
            errors.report("patch", &error, metrics)?;
            Renewal::Failed { version: None }
        }
    };
    Ok((renewal, began))
}

/// The read after a renewal's conflict, and the one renewal it may allow.
async fn after_conflict(
    api: &impl LeaseApi,
    me: &str,
    t: &Timings,
    metrics: &Metrics,
    errors: &LeaseErrors,
) -> io::Result<Renewal> {
    let lease = match bounded(t.call_timeout, api.get()).await {
        Ok(lease) => {
            errors.ok("get");
            lease
        }
        Err(error) => {
            errors.report("get", &error, metrics)?;
            return Ok(Renewal::Failed { version: None });
        }
    };
    let Some(observed) = pure::observe(&lease) else {
        errors.report("get", &no_version(), metrics)?;
        return Ok(Renewal::Failed { version: None });
    };
    let version = match pure::after_conflict(&observed, me) {
        AfterConflict::HeldByAnother => return Ok(Renewal::HeldByAnother),
        AfterConflict::RenewWith(version) => version,
    };
    let sent = Instant::now();
    let body = pure::renew_body(&version, me, Timestamp::now());
    Ok(match bounded(t.call_timeout, api.patch(&body)).await {
        Ok(written) => {
            errors.ok("patch");
            Renewal::Renewed {
                version: pure::observe(&written).map(|o| o.version),
                sent,
            }
        }
        Err(error) => {
            errors.report("patch", &error, metrics)?;
            Renewal::Failed {
                version: Some(version),
            }
        }
    })
}

/// Releases the Lease at shutdown: one release patch with `version`, within `release_timeout`,
/// logged as `lease released` or `lease release failed`. The caller sends it only while its term
/// holds. Returns only an unwritable log as an error.
pub async fn release(
    api: &impl LeaseApi,
    version: &str,
    lease: &str,
    t: &Timings,
) -> io::Result<()> {
    let body = pure::release_body(version, Timestamp::now());
    match bounded(t.release_timeout, api.patch(&body)).await {
        Ok(_) => log::info("lease released", json!({"lease": lease})),
        Err(error) => log::warn(
            "lease release failed",
            json!({"lease": lease, "error": error.to_string()}),
        ),
    }
}

#[cfg(test)]
mod tests {
    // Acceptance tests for the election driver (docs/adr/0019-high-availability.md), written before
    // the code, over a scripted `LeaseApi` with short timings and real time on a current-thread
    // runtime (no tokio test-util). Values are invented, and no failure message prints a value read
    // from a Lease.

    use super::pure::{Timings, release_body, renew_body};
    use super::*;

    use std::cell::{Cell, RefCell};
    use std::future::Future;
    use std::time::{Duration, Instant};

    use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use k8s_openapi::jiff::Timestamp;
    use serde_json::{Value, json};

    use crate::metrics::Metrics;

    const ME: &str = "kausa-7d9c8b6f4d-2xkqp";
    const OTHER: &str = "kausa-7d9c8b6f4d-9wzzt";
    const LEASE: &str = "kausa/kausa-leader";

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// 300/200/50 ms and 50 ms calls: the production ratios, a hundred times faster.
    const T: Timings = Timings {
        lease_duration: Duration::from_millis(300),
        renew_deadline: Duration::from_millis(200),
        retry_period: Duration::from_millis(50),
        call_timeout: Duration::from_millis(50),
        release_timeout: Duration::from_millis(50),
    };

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    fn lease(version: &str, holder: Option<&str>, transitions: Option<i32>) -> Lease {
        Lease {
            metadata: ObjectMeta {
                name: Some("kausa-leader".into()),
                namespace: Some("kausa".into()),
                resource_version: Some(version.into()),
                ..ObjectMeta::default()
            },
            spec: Some(LeaseSpec {
                holder_identity: holder.map(str::to_owned),
                lease_transitions: transitions,
                ..LeaseSpec::default()
            }),
        }
    }

    /// What one scripted call answers.
    enum Answer {
        Lease(Box<Lease>),
        Error(LeaseError),
        /// Never answers.
        Hang,
    }

    impl Answer {
        async fn resolve(self) -> Result<Lease, LeaseError> {
            match self {
                Answer::Lease(lease) => Ok(*lease),
                Answer::Error(error) => Err(error),
                Answer::Hang => std::future::pending().await,
            }
        }
    }

    type Script = Box<dyn Fn(usize) -> Answer>;

    /// A Lease API whose `n`-th get and `n`-th patch answer what the scripts say. It records when
    /// each call began and every patch body.
    struct Scripted {
        get: Script,
        patch: Script,
        gets: Cell<usize>,
        patches: RefCell<Vec<(Instant, Value)>>,
    }

    impl Scripted {
        fn new(
            get: impl Fn(usize) -> Answer + 'static,
            patch: impl Fn(usize) -> Answer + 'static,
        ) -> Self {
            Self {
                get: Box::new(get),
                patch: Box::new(patch),
                gets: Cell::new(0),
                patches: RefCell::new(Vec::new()),
            }
        }

        fn bodies(&self) -> Vec<Value> {
            self.patches
                .borrow()
                .iter()
                .map(|(_, b)| b.clone())
                .collect()
        }
    }

    impl LeaseApi for Scripted {
        async fn get(&self) -> Result<Lease, LeaseError> {
            let n = self.gets.get();
            self.gets.set(n + 1);
            (self.get)(n).resolve().await
        }

        async fn patch(&self, body: &Value) -> Result<Lease, LeaseError> {
            let n = {
                let mut patches = self.patches.borrow_mut();
                patches.push((Instant::now(), body.clone()));
                patches.len() - 1
            };
            (self.patch)(n).resolve().await
        }
    }

    fn no_get(_: usize) -> Answer {
        panic!("no get expected")
    }

    fn no_patch(_: usize) -> Answer {
        panic!("no patch expected")
    }

    /// The value of one series in `/metrics`, 0 when it was never incremented.
    fn counter(metrics: &Metrics, series: &str) -> u64 {
        let text = metrics.render();
        text.lines()
            .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
            .map_or(0, |value| value.parse().expect("a count"))
    }

    const GET_ERRORS: &str = "kausa_lease_errors_total{call=\"get\"}";
    const PATCH_ERRORS: &str = "kausa_lease_errors_total{call=\"patch\"}";

    fn errors() -> LeaseErrors {
        LeaseErrors::new(LEASE, Duration::from_secs(60))
    }

    fn version_of(body: &Value) -> &str {
        body["metadata"]["resourceVersion"]
            .as_str()
            .expect("a resourceVersion precondition")
    }

    /// Checks the shape of an acquire body; the times are checked as MicroTimes by pure.rs.
    fn assert_acquire(body: &Value, version: &str, transitions: i32) {
        assert_eq!(version_of(body), version);
        let spec = &body["spec"];
        assert_eq!(spec["holderIdentity"], json!(ME));
        assert_eq!(spec["leaseDurationSeconds"], json!(30));
        assert_eq!(spec["leaseTransitions"], json!(transitions));
        assert!(spec["acquireTime"].is_string());
        assert_eq!(spec["acquireTime"], spec["renewTime"]);
        assert_eq!(body.as_object().map(|o| o.len()), Some(2));
    }

    /// Checks that a body is a renewal: the same keys as `renew_body`, with `version`.
    fn assert_renew(body: &Value, version: &str) {
        let template = renew_body(version, ME, Timestamp::UNIX_EPOCH);
        assert_eq!(version_of(body), version);
        assert_eq!(body["spec"]["holderIdentity"], json!(ME));
        let keys = |v: &Value| {
            v["spec"]
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
        };
        assert_eq!(keys(body), keys(&template));
        assert_eq!(body["metadata"], template["metadata"]);
    }

    // --- Term ------------------------------------------------------------------------------------

    #[test]
    fn a_term_holds_until_its_end() {
        assert!(Term::unbounded().holds());
        assert!(!Term::closed().holds());
        let term = Term::closed();
        term.extend_to(Instant::now() + Duration::from_secs(60));
        assert!(term.holds());
        term.close();
        assert!(!term.holds());
        term.extend_to(Instant::now() + ms(30));
        assert!(term.holds());
        std::thread::sleep(ms(40));
        assert!(!term.holds(), "a term ends on its own, with no timer");
        let unbounded = Term::unbounded();
        unbounded.close();
        assert!(!unbounded.holds(), "a step-down closes any term");
    }

    // --- standby ---------------------------------------------------------------------------------

    #[test]
    fn a_free_lease_is_acquired_at_the_first_read() {
        let api = Scripted::new(
            |n| match n {
                0 => Answer::Lease(Box::new(lease("1", None, None))),
                _ => panic!("one read is enough"),
            },
            |n| match n {
                0 => Answer::Lease(Box::new(lease("2", Some(ME), Some(1)))),
                _ => panic!("one acquire is enough"),
            },
        );
        let metrics = Metrics::default();
        let started = Instant::now();
        let acquired = block_on(standby(&api, ME, &T, &metrics, &errors())).expect("the log");
        let elapsed = started.elapsed();
        assert!(elapsed < T.retry_period, "{elapsed:?}");
        assert_eq!(acquired.version, "2", "the version the acquire returned");
        assert!(acquired.sent >= started && acquired.sent <= Instant::now());
        let bodies = api.bodies();
        assert_eq!(bodies.len(), 1);
        assert_acquire(&bodies[0], "1", 1);
        // Acquired at the first read: not Ready until its watches have listed.
        assert!(!metrics.is_ready());
    }

    #[test]
    fn a_changing_holder_is_never_taken() {
        let api = Scripted::new(
            |n| {
                Answer::Lease(Box::new(lease(
                    &format!("{}", 10 + n),
                    Some(OTHER),
                    Some(2),
                )))
            },
            no_patch,
        );
        let metrics = Metrics::default();
        let ended = block_on(async {
            tokio::time::timeout(ms(800), standby(&api, ME, &T, &metrics, &errors())).await
        });
        assert!(ended.is_err(), "a Lease renewed by another is never taken");
        assert!(api.patches.borrow().is_empty());
        // A read every retry period, the first at once.
        let gets = api.gets.get();
        assert!((12..=17).contains(&gets), "{gets} reads in 800 ms");
        // A successful read that leaves this replica a standby makes it Ready.
        assert!(metrics.is_ready());
    }

    #[test]
    fn an_unchanged_foreign_holder_is_taken_after_the_lease_duration() {
        let api = Scripted::new(
            |_| Answer::Lease(Box::new(lease("7", Some(OTHER), Some(3)))),
            |n| match n {
                0 => Answer::Lease(Box::new(lease("8", Some(ME), Some(4)))),
                _ => panic!("one acquire is enough"),
            },
        );
        let metrics = Metrics::default();
        let started = Instant::now();
        let acquired = block_on(standby(&api, ME, &T, &metrics, &errors())).expect("the log");
        let elapsed = started.elapsed();
        assert!(elapsed >= T.lease_duration, "taken after {elapsed:?}");
        assert!(
            elapsed < T.lease_duration + 2 * T.retry_period + ms(30),
            "taken after {elapsed:?}"
        );
        assert_eq!(acquired.version, "8");
        let bodies = api.bodies();
        assert_eq!(bodies.len(), 1);
        assert_acquire(&bodies[0], "7", 4);
        assert!(metrics.is_ready(), "it waited as a standby first");
    }

    #[test]
    fn a_conflict_on_acquire_stays_standby() {
        let api = Scripted::new(
            |n| match n {
                0 => Answer::Lease(Box::new(lease("1", None, None))),
                // Whatever the next reads say, only the conflict can have made it Ready.
                _ => Answer::Hang,
            },
            |n| match n {
                0 => Answer::Error(LeaseError::Conflict),
                _ => panic!("no second acquire"),
            },
        );
        let metrics = Metrics::default();
        let ended = block_on(async {
            tokio::time::timeout(ms(500), standby(&api, ME, &T, &metrics, &errors())).await
        });
        assert!(
            ended.is_err(),
            "the replica that lost the race stays a standby"
        );
        assert_eq!(api.patches.borrow().len(), 1);
        assert!(
            metrics.is_ready(),
            "a conflict on acquire leaves a Ready standby"
        );
        // A conflict is not an error.
        assert_eq!(counter(&metrics, PATCH_ERRORS), 0);
    }

    #[test]
    fn a_refused_read_is_counted_every_time_and_never_makes_the_replica_ready() {
        let api = Scripted::new(
            |_| Answer::Error(LeaseError::Forbidden("an invented refusal".into())),
            no_patch,
        );
        let metrics = Metrics::default();
        let ended = block_on(async {
            tokio::time::timeout(ms(280), standby(&api, ME, &T, &metrics, &errors())).await
        });
        assert!(ended.is_err());
        let gets = api.gets.get();
        assert!(gets >= 4, "{gets} reads");
        assert_eq!(counter(&metrics, GET_ERRORS), gets as u64);
        assert_eq!(counter(&metrics, PATCH_ERRORS), 0);
        assert!(api.patches.borrow().is_empty());
        assert!(!metrics.is_ready(), "Ready needs a successful read");
    }

    #[test]
    fn a_missing_lease_is_never_created() {
        let api = Scripted::new(|_| Answer::Error(LeaseError::NotFound), no_patch);
        let metrics = Metrics::default();
        let ended = block_on(async {
            tokio::time::timeout(ms(200), standby(&api, ME, &T, &metrics, &errors())).await
        });
        assert!(ended.is_err(), "with no Lease nobody leads");
        assert!(api.patches.borrow().is_empty());
        assert_eq!(counter(&metrics, GET_ERRORS), api.gets.get() as u64);
    }

    // --- hold ------------------------------------------------------------------------------------

    fn acquired(version: &str) -> Acquired {
        Acquired {
            version: version.into(),
            sent: Instant::now(),
        }
    }

    #[test]
    fn hold_renews_every_retry_period_with_the_version_each_renewal_returned() {
        let api = Scripted::new(no_get, |n| {
            Answer::Lease(Box::new(lease(&format!("{}", 100 + n), Some(ME), Some(1))))
        });
        let metrics = Metrics::default();
        let term = Term::closed();
        let ended = block_on(async {
            tokio::time::timeout(
                ms(520),
                hold(&api, ME, acquired("2"), &term, &T, &metrics, &errors()),
            )
            .await
        });
        assert!(ended.is_err(), "renewals that succeed keep the Lease");
        assert!(term.holds(), "each renewal extends the term");
        let patches = api.patches.borrow();
        assert!(
            (7..=11).contains(&patches.len()),
            "{} renewals",
            patches.len()
        );
        for pair in patches.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            assert!(gap >= ms(45), "renewals {gap:?} apart");
        }
        for (n, (_, body)) in patches.iter().enumerate() {
            let expected = if n == 0 {
                "2".to_owned()
            } else {
                format!("{}", 100 + n - 1)
            };
            assert_renew(body, &expected);
        }
    }

    #[test]
    fn a_conflict_still_naming_this_pod_renews_with_the_new_version() {
        // A Helm label change moved the version.
        let api = Scripted::new(
            |n| match n {
                0 => Answer::Lease(Box::new(lease("9", Some(ME), Some(1)))),
                _ => panic!("one read after a conflict"),
            },
            |n| match n {
                0 => Answer::Error(LeaseError::Conflict),
                _ => Answer::Lease(Box::new(lease(&format!("{}", 200 + n), Some(ME), Some(1)))),
            },
        );
        let metrics = Metrics::default();
        let term = Term::closed();
        let ended = block_on(async {
            tokio::time::timeout(
                ms(130),
                hold(&api, ME, acquired("2"), &term, &T, &metrics, &errors()),
            )
            .await
        });
        assert!(ended.is_err(), "still named: kausa keeps the Lease");
        assert_eq!(api.gets.get(), 1);
        let patches = api.patches.borrow();
        assert!(patches.len() >= 2, "the renewal is sent again");
        assert_renew(&patches[0].1, "2");
        assert_renew(&patches[1].1, "9");
        assert!(
            patches[1].0 - patches[0].0 < T.retry_period,
            "the renewal with the new version is sent at once"
        );
        if let Some((_, third)) = patches.get(2) {
            assert_renew(third, "201");
        }
        assert!(term.holds());
        assert_eq!(counter(&metrics, GET_ERRORS), 0);
        assert_eq!(counter(&metrics, PATCH_ERRORS), 0);
    }

    #[test]
    fn another_holder_ends_hold_at_once() {
        for holder in [Some(OTHER), Some(""), None] {
            let api = Scripted::new(
                move |n| match n {
                    0 => Answer::Lease(Box::new(lease("9", holder, Some(2)))),
                    _ => panic!("one read after a conflict"),
                },
                |n| match n {
                    0 => Answer::Error(LeaseError::Conflict),
                    _ => panic!("no renewal of a Lease held by another"),
                },
            );
            let metrics = Metrics::default();
            let term = Term::closed();
            let started = Instant::now();
            let lost = block_on(async {
                tokio::time::timeout(
                    ms(1000),
                    hold(&api, ME, acquired("2"), &term, &T, &metrics, &errors()),
                )
                .await
            })
            .expect("hold ends")
            .expect("the log");
            let elapsed = started.elapsed();
            assert!(
                matches!(lost, Lost::HeldByAnother),
                "holder {:?}",
                holder.map(str::len)
            );
            assert!(
                elapsed < T.retry_period + ms(40),
                "it ended after {elapsed:?}, not at once"
            );
            assert_eq!(api.patches.borrow().len(), 1);
        }
    }

    #[test]
    fn a_patch_that_never_returns_ends_hold_by_the_deadline() {
        let api = Scripted::new(no_get, |_| Answer::Hang);
        let metrics = Metrics::default();
        let term = Term::closed();
        let given = acquired("2");
        let sent = given.sent;
        let lost = block_on(async {
            tokio::time::timeout(
                ms(2000),
                hold(&api, ME, given, &term, &T, &metrics, &errors()),
            )
            .await
        })
        .expect("a hung call never delays the step-down")
        .expect("the log");
        let elapsed = sent.elapsed();
        assert!(matches!(lost, Lost::NotRenewed));
        assert!(elapsed >= T.renew_deadline, "ended after {elapsed:?}");
        assert!(
            elapsed < T.renew_deadline + T.retry_period,
            "ended after {elapsed:?}"
        );
        assert!(!term.holds(), "the term ends at the deadline");
    }

    #[test]
    fn failing_renewals_are_counted_and_end_hold_by_the_deadline() {
        for (case, error) in [
            (
                "forbidden",
                (|| LeaseError::Forbidden("an invented refusal".into())) as fn() -> LeaseError,
            ),
            ("not found", || LeaseError::NotFound),
            ("other", || LeaseError::Other("an invented failure".into())),
            ("timeout", || LeaseError::Timeout),
        ] {
            let api = Scripted::new(no_get, move |_| Answer::Error(error()));
            let metrics = Metrics::default();
            let term = Term::closed();
            let given = acquired("2");
            let sent = given.sent;
            let lost = block_on(async {
                tokio::time::timeout(
                    ms(2000),
                    hold(&api, ME, given, &term, &T, &metrics, &errors()),
                )
                .await
            })
            .expect("hold ends")
            .expect("the log");
            let elapsed = sent.elapsed();
            assert!(matches!(lost, Lost::NotRenewed), "{case}");
            assert!(
                elapsed >= T.renew_deadline && elapsed < T.renew_deadline + T.retry_period,
                "{case}: ended after {elapsed:?}"
            );
            let patches = api.patches.borrow().len();
            assert!(patches >= 3, "{case}: {patches} renewals");
            assert_eq!(counter(&metrics, PATCH_ERRORS), patches as u64, "{case}");
            assert_eq!(counter(&metrics, GET_ERRORS), 0, "{case}");
        }
    }

    // --- The lease error log ---------------------------------------------------------------------

    #[test]
    fn lease_errors_are_logged_when_they_start_change_or_once_per_period() {
        let metrics = Metrics::default();
        let errors = LeaseErrors::new(LEASE, ms(100));
        let forbidden = |text: &str| LeaseError::Forbidden(text.into());
        let report = |error: &LeaseError| errors.report("get", error, &metrics).expect("the log");
        assert!(report(&forbidden("a")), "an error that starts is logged");
        assert!(!report(&forbidden("a")), "the same error again is not");
        assert!(!report(&forbidden("a")));
        assert!(report(&forbidden("b")), "a new text is logged");
        assert!(report(&LeaseError::NotFound), "another error is logged");
        assert!(!report(&LeaseError::NotFound));
        std::thread::sleep(ms(120));
        assert!(
            report(&LeaseError::NotFound),
            "once per period while it lasts"
        );
        assert!(!report(&LeaseError::NotFound));
        // A success ends the error: the next one starts again.
        errors.ok("get");
        assert!(report(&LeaseError::NotFound));
        // Every one is counted, logged or not.
        assert_eq!(counter(&metrics, GET_ERRORS), 9);
        assert_eq!(counter(&metrics, PATCH_ERRORS), 0);
    }

    #[test]
    fn a_refusal_and_a_missing_lease_say_what_to_do() {
        assert_eq!(
            hint(&LeaseError::Forbidden("an invented refusal".into())),
            Some("kausa needs get and patch on this Lease (docs/reference/permissions.md)")
        );
        assert_eq!(
            hint(&LeaseError::NotFound),
            Some("the chart creates this Lease: run helm upgrade")
        );
        for error in [
            LeaseError::Other("an invented failure".into()),
            LeaseError::Timeout,
            LeaseError::Conflict,
        ] {
            assert_eq!(hint(&error), None);
        }
    }

    // --- release ---------------------------------------------------------------------------------

    #[test]
    fn release_sends_one_release_body_with_the_last_version() {
        let api = Scripted::new(no_get, |n| match n {
            0 => Answer::Lease(Box::new(lease("31", Some(""), Some(1)))),
            _ => panic!("one release"),
        });
        block_on(release(&api, "30", LEASE, &T)).expect("the log");
        let bodies = api.bodies();
        assert_eq!(bodies.len(), 1);
        let template = release_body("30", Timestamp::UNIX_EPOCH);
        assert_eq!(bodies[0]["metadata"], template["metadata"]);
        assert_eq!(bodies[0]["spec"]["holderIdentity"], json!(""));
        assert_eq!(bodies[0]["spec"]["leaseDurationSeconds"], json!(1));
        assert!(bodies[0]["spec"]["renewTime"].is_string());
        assert_eq!(
            bodies[0]["spec"].as_object().map(|o| o.len()),
            template["spec"].as_object().map(|o| o.len())
        );
    }

    #[test]
    fn a_failed_or_hung_release_is_tried_once_within_its_timeout() {
        for (case, answer) in [
            (
                "conflict",
                (|| Answer::Error(LeaseError::Conflict)) as fn() -> Answer,
            ),
            ("refused", || {
                Answer::Error(LeaseError::Forbidden("an invented refusal".into()))
            }),
            ("hung", || Answer::Hang),
        ] {
            let api = Scripted::new(no_get, move |_| answer());
            let started = Instant::now();
            block_on(release(&api, "30", LEASE, &T)).expect("the log");
            let elapsed = started.elapsed();
            assert!(
                elapsed < T.release_timeout + ms(30),
                "{case}: released after {elapsed:?}"
            );
            assert_eq!(api.patches.borrow().len(), 1, "{case}");
        }
    }

    // --- Beyond the acceptance tests -------------------------------------------------------------

    #[test]
    fn the_term_keeps_the_version_to_release_with_while_it_holds() {
        let api = Scripted::new(
            |n| match n {
                0 => Answer::Lease(Box::new(lease("40", Some(OTHER), Some(2)))),
                _ => panic!("one read after a conflict"),
            },
            |n| match n {
                0 => Answer::Lease(Box::new(lease("31", Some(ME), Some(1)))),
                1 => Answer::Error(LeaseError::Conflict),
                _ => panic!("no renewal of a Lease held by another"),
            },
        );
        let metrics = Metrics::default();
        let term = Term::closed();
        assert_eq!(term.version(), None);
        let errors = errors();
        let lost = block_on(async {
            let holding = hold(&api, ME, acquired("30"), &term, &T, &metrics, &errors);
            let mut holding = std::pin::pin!(holding);
            // Before the first renewal: the acquire's version.
            assert!(
                tokio::time::timeout(ms(20), holding.as_mut())
                    .await
                    .is_err()
            );
            assert_eq!(term.version().as_deref(), Some("30"));
            // After it: the version it returned.
            assert!(
                tokio::time::timeout(ms(50), holding.as_mut())
                    .await
                    .is_err()
            );
            assert_eq!(term.version().as_deref(), Some("31"));
            holding.await.expect("the log")
        });
        assert_eq!(lost, Lost::HeldByAnother);
        assert!(!term.holds());
        assert_eq!(term.version(), None, "a closed term has nothing to release");
    }

    #[test]
    fn answers_of_the_api_server_are_told_by_code_and_reason_only() {
        let api = |code: u16| {
            kube::Error::Api(Box::new(
                kube::core::Status::failure("an invented message", "AnInventedReason")
                    .with_code(code),
            ))
        };
        assert_eq!(LeaseError::from_kube(&api(409)), LeaseError::Conflict);
        assert_eq!(LeaseError::from_kube(&api(404)), LeaseError::NotFound);
        assert!(matches!(
            LeaseError::from_kube(&api(403)),
            LeaseError::Forbidden(_)
        ));
        for code in [403, 422, 500] {
            let text = LeaseError::from_kube(&api(code)).to_string();
            // The message can quote the stored Lease, so it is never logged.
            assert!(!text.contains("an invented message"), "{code}");
            assert!(
                text.starts_with(&format!("{code} AnInventedReason")),
                "{code}"
            );
        }
        let undecodable = serde_json::from_str::<Value>("{\"an invented\": ").expect_err("bad");
        let text = LeaseError::from_kube(&kube::Error::SerdeError(undecodable)).to_string();
        assert!(
            !text.contains("invented"),
            "a decoding error can quote the answer"
        );
    }

    #[test]
    fn a_lasting_lease_error_is_logged_again_once_a_minute() {
        assert_eq!(LEASE_ERROR_PERIOD, Duration::from_secs(60));
    }

    // --- KubeLease, against a fake API server ----------------------------------------------------

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A fake API server: one connection per request, answered in order with a status line and a
    /// JSON body, or (`None`) held open unanswered. Each request's line, head and body is sent back.
    fn api_server(
        answers: Vec<Option<(&'static str, Value)>>,
    ) -> (String, mpsc::Receiver<(String, String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for answer in answers {
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
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length || n == 0 {
                            let (line, head) = text[..end].split_once("\r\n").unwrap_or_default();
                            let _ = sent.send((
                                line.to_owned(),
                                head.to_ascii_lowercase(),
                                text[end + 4..].to_owned(),
                            ));
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let Some((status, body)) = answer else {
                    held.push(stream);
                    continue;
                };
                let body = body.to_string();
                let answer = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(answer.as_bytes()).expect("a write");
            }
            std::thread::sleep(Duration::from_secs(2));
        });
        (url, received)
    }

    fn kube_lease(url: &str, timeout: Duration) -> KubeLease {
        let config = kube::Config::new(url.parse().expect("a URL"));
        let client = Client::try_from(config).expect("a client");
        KubeLease::new(client, "kausa", "kausa-leader", timeout)
    }

    fn stored(version: &str) -> Value {
        json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": {"name": "kausa-leader", "namespace": "kausa", "resourceVersion": version},
            "spec": {},
        })
    }

    fn status(code: u16, reason: &str) -> Value {
        json!({
            "apiVersion": "v1",
            "kind": "Status",
            "status": "Failure",
            "message": "an invented message",
            "reason": reason,
            "code": code,
        })
    }

    const PATH: &str = "/apis/coordination.k8s.io/v1/namespaces/kausa/leases/kausa-leader";

    #[test]
    fn kube_lease_gets_and_merge_patches_its_one_lease_with_strict_validation() {
        let (url, sent) = api_server(vec![
            Some(("200 OK", stored("5"))),
            Some(("200 OK", stored("6"))),
        ]);
        let body = renew_body("5", ME, Timestamp::UNIX_EPOCH);
        let (read, written) = block_on(async {
            let api = kube_lease(&url, Duration::from_secs(5));
            (api.get().await, api.patch(&body).await)
        });
        let version = |lease: &Lease| lease.metadata.resource_version.clone();
        assert_eq!(read.as_ref().map(version), Ok(Some("5".into())));
        assert_eq!(written.as_ref().map(version), Ok(Some("6".into())));
        let (line, _, _) = sent.recv().expect("the get");
        assert_eq!(line, format!("GET {PATH} HTTP/1.1"));
        let (line, head, sent_body) = sent.recv().expect("the patch");
        let query = line
            .strip_prefix(&format!("PATCH {PATH}?"))
            .and_then(|rest| rest.strip_suffix(" HTTP/1.1"))
            .expect("a patch of the Lease");
        assert!(
            query.split('&').any(|p| p == "fieldValidation=Strict"),
            "{line}"
        );
        assert!(head.contains("content-type: application/merge-patch+json"));
        assert_eq!(
            serde_json::from_str::<Value>(&sent_body).expect("a JSON body"),
            body
        );
    }

    #[test]
    fn kube_lease_tells_conflicts_refusals_and_a_missing_lease_apart() {
        let (url, _sent) = api_server(vec![
            Some(("409 Conflict", status(409, "Conflict"))),
            Some(("404 Not Found", status(404, "NotFound"))),
            Some(("403 Forbidden", status(403, "Forbidden"))),
            Some(("500 Internal Server Error", status(500, "InternalError"))),
        ]);
        let body = renew_body("5", ME, Timestamp::UNIX_EPOCH);
        let errors = block_on(async {
            let api = kube_lease(&url, Duration::from_secs(5));
            vec![
                api.patch(&body).await.expect_err("a conflict"),
                api.get().await.expect_err("a missing Lease"),
                api.get().await.expect_err("a refusal"),
                api.get().await.expect_err("a failure"),
            ]
        });
        assert_eq!(errors[0], LeaseError::Conflict);
        assert_eq!(errors[1], LeaseError::NotFound);
        assert!(matches!(errors[2], LeaseError::Forbidden(_)));
        assert!(matches!(errors[3], LeaseError::Other(_)));
        assert!(
            errors
                .iter()
                .all(|e| !e.to_string().contains("an invented message"))
        );
    }

    #[test]
    fn kube_lease_gives_up_on_a_silent_api_server_at_its_timeout() {
        let (url, _sent) = api_server(vec![None]);
        let (read, elapsed) = block_on(async {
            let api = kube_lease(&url, ms(200));
            let started = Instant::now();
            (api.get().await, started.elapsed())
        });
        assert_eq!(read.map(|_| ()), Err(LeaseError::Timeout));
        assert!(elapsed >= ms(200) && elapsed < ms(1000), "{elapsed:?}");
    }
}
