//! `kausa run`: the controller loop. It watches Pods, Warning events and, unless `--no-rollouts`,
//! Deployments, keeps small records, correlates owners and logs each incident when it settles.
//!
//! Status as of 2026-10-30 (v0.4.0, with the Deployment watch and the standby): incidents are
//! written to kausa's log and, with `--endpoint` or `--dry-run`, explained by the model or their
//! request logged (`explain.rs`); with `--endpoint` and without `--no-events`, the explanation is also an
//! Event (`events.rs`). The API calls are the watches (one of Pods, one of Warning events and,
//! unless `--no-rollouts`, one of Deployments on the whole cluster, or one of each per watched
//! namespace; docs/adr/0018-stuck-rollouts.md), the metadata reads of ReplicaSets and
//! Jobs, `pods/log` for `D001`, `D003` and `D008` unless `--no-logs`, the Event writes and, with
//! `--history`, the Explanation writes and their sweep, at start and hourly in the explanation
//! slot (docs/reference/permissions.md, docs/adr/0014-explanation-history.md).
//! With `--notify-url-file`, each new explanation is also posted to the webhook, one at a time,
//! from a slot of its own beside the explanation's (`notify.rs`,
//! docs/adr/0015-notifications.md). `--metrics-address` serves the counters (`metrics.rs`).
//!
//! With `--leader-lease`, only the holder of that Lease acts (docs/adr/0019-high-availability.md):
//! `run` keeps what lives across terms (the metrics and their listener, the term, the budget and
//! the deduplication cache), a standby reads the Lease and nothing else, and each term of the
//! leader runs `lead`, whose watches, records, queues and owner cache are dropped when it ends.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::fmt::Debug;
use std::future::Future;
use std::io;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use futures::stream::{self, BoxStream, FuturesUnordered, SelectAll};
use futures::{Stream, StreamExt};
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::api::events::v1::Event;
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use k8s_openapi::serde::de::DeserializeOwned;
use kube::config::KubeConfigOptions;
use kube::core::NamespaceResourceScope;
use kube::runtime::utils::Backoff;
use kube::runtime::watcher;
use kube::{Api, Client, Config, Resource};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::signal::unix::{SignalKind, signal};

use crate::budget::{Budget, Dedup};
use crate::context::Gathered;
use crate::detect::{Detection, detect_event, detect_rollout};
use crate::events::Writer;
use crate::explain::{self, LogSource, QUEUE_LIMIT, Settings, Shared, Work};
use crate::history::{self, History};
use crate::incident::{EventView, Incident, PodView, Tracker, collect_signals, rollout_signals};
use crate::leader::pure::{TIMINGS, Timings};
use crate::leader::{self, KubeLease, LEASE_ERROR_PERIOD, LeaseApi, LeaseErrors, Lost, Term};
use crate::log;
use crate::metrics::{self, Metrics};
use crate::notify::{self, Notifier};
use crate::owner::{Hop, OwnerLookup, top_owner};
use crate::record::{DeploymentRecord, EventRecord, OwnerRef, PodRecord, controller_of};

/// The reporting controller of the Events kausa writes, excluded from its own watch.
pub const REPORTING_CONTROLLER: &str = "kausa";

/// How often the current state is turned into signals.
const TICK: Duration = Duration::from_secs(5);
/// How long a Warning event counts as a current signal after it last happened. Chosen longer than
/// the interval at which client-go's event recorder is thought to update a repeated event (about
/// five minutes at most; a hypothesis to verify on a live cluster), so that a probe that keeps
/// failing stays a signal between two updates of its event.
pub const EVENT_FRESHNESS: SignedDuration = SignedDuration::from_mins(10);
/// How long an owner lookup is reused, and how long a failed one is not retried.
const OWNER_CACHE_TTL: Duration = Duration::from_secs(600);
const OWNER_ERROR_TTL: Duration = Duration::from_secs(30);
/// How many metrics connections are answered at once; more wait in the listen backlog.
const MAX_CONNECTIONS: usize = 8;
/// How long to wait after a failed accept (for example, out of file descriptors).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// How long one owner lookup may take before it counts as failed.
const OWNER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Options {
    pub settle: SignedDuration,
    /// How to explain incidents; `None` logs them only.
    pub explain: Option<Settings>,
    /// Where to serve `/metrics`, `/healthz` and `/readyz`, if anywhere.
    pub metrics_address: Option<String>,
    /// The namespaces to watch, in the order given; empty watches the whole cluster.
    pub namespaces: Vec<String>,
    /// Whether to watch Deployments for stalled rollouts (`D014`); `--no-rollouts` turns it off
    /// (docs/adr/0018-stuck-rollouts.md).
    pub rollouts: bool,
    /// With `--leader-lease`, the Lease this replica acts under: it acts only while it holds it,
    /// and waits as a standby otherwise (docs/adr/0019-high-availability.md).
    pub leader_lease: Option<LeaderLease>,
}

/// The Lease of `--leader-lease` and this replica's identity in it.
pub struct LeaderLease {
    pub namespace: String,
    pub name: String,
    /// `POD_NAME`, compared byte for byte with the Lease's holder.
    pub identity: String,
}

impl LeaderLease {
    /// The flag's own `<namespace>/<name>`, the only name of the Lease that kausa logs.
    fn flag(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

/// Which watch an item comes from: `None` is the cluster-wide watch, `Some(n)` the watch of
/// namespace `n` (docs/adr/0013-namespaced-install-mode.md).
pub type WatchKey = Option<String>;

/// Which kind of object a watch reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Pods,
    Events,
    Deployments,
}

/// Whether a record in `namespace` comes from `watch`: every record does for the cluster-wide
/// watch, and only its namespace's records for a namespace's watch.
fn inside(watch: &WatchKey, namespace: &str) -> bool {
    watch.as_deref().is_none_or(|n| n == namespace)
}

/// Whether a watch error is the API server refusing the list or the watch (`403`). The status
/// code decides, not the reason (docs/adr/0013-namespaced-install-mode.md).
fn forbidden(error: &watcher::Error) -> bool {
    match error {
        watcher::Error::InitialListFailed(kube::Error::Api(status))
        | watcher::Error::WatchStartFailed(kube::Error::Api(status))
        | watcher::Error::WatchError(status) => status.code == 403,
        _ => false,
    }
}

/// Which watches have sent `InitDone` at least once. kausa is ready once every watch has: 3 on
/// the whole cluster, or 3 per namespace, and 2 for each with `--no-rollouts`
/// (docs/adr/0013-namespaced-install-mode.md, docs/adr/0018-stuck-rollouts.md).
struct Readiness {
    total: usize,
    listed: HashSet<(Kind, WatchKey)>,
}

impl Readiness {
    /// The watches `run` starts for `namespaces`: one Pod, one event and, with `rollouts`, one
    /// Deployment watch per namespace, or one of each on the whole cluster when the list is empty.
    fn new(namespaces: &[String], rollouts: bool) -> Self {
        Readiness {
            total: (2 + usize::from(rollouts)) * namespaces.len().max(1),
            listed: HashSet::new(),
        }
    }

    /// Records that `watch` of `kind` has listed, and says whether every watch now has. A repeated
    /// `InitDone` of one watch counts once, and readiness never goes back.
    fn listed(&mut self, kind: Kind, watch: &WatchKey) -> bool {
        self.listed.insert((kind, watch.clone()));
        self.listed.len() >= self.total
    }
}

/// The error of a watch that ended: kausa stops, since a watcher with a backoff only ends when
/// its stream does.
fn ended(kind: Kind, watch: &WatchKey) -> RunError {
    let kind = match kind {
        Kind::Pods => "pod",
        Kind::Events => "event",
        Kind::Deployments => "deployment",
    };
    RunError::Failure(match watch {
        None => format!("the {kind} watch ended"),
        Some(namespace) => format!("the {kind} watch of namespace {namespace} ended"),
    })
}

/// An item of one watch, tagged with its watch; `None` once that watch has ended.
type Tagged<K> = (WatchKey, Option<Result<watcher::Event<K>, watcher::Error>>);

/// The watches of one kind, merged: one on the whole cluster, or one per namespace.
type Watches<K> = SelectAll<BoxStream<'static, Tagged<K>>>;

/// One watch that starts over with a fresh watcher after a `403`, so that it lists again once
/// access is back: kube-runtime 4.2.0 relists only after a failed list, and resumes a refused
/// watch (`WatchStartFailed`, or an error in the open stream) from its resource version, which
/// would never bring back the objects that did not change while `State` had dropped them
/// (docs/adr/0013-namespaced-install-mode.md). After every error it waits for `backoff` before
/// the next poll, as `StreamBackoff` does, but `Init` does not reset the delay: every fresh
/// watcher sends `Init` before it lists, so a refused namespace still backs off to the longest
/// delay instead of being retried at the shortest one.
struct Restarting<S, F, B> {
    start: F,
    watch: Option<S>,
    backoff: B,
    delay: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<S, F, B> Restarting<S, F, B> {
    fn new(start: F, backoff: B) -> Self {
        Restarting {
            start,
            watch: None,
            backoff,
            delay: None,
        }
    }
}

impl<K, S, F, B> Stream for Restarting<S, F, B>
where
    S: Stream<Item = Result<watcher::Event<K>, watcher::Error>> + Unpin,
    F: FnMut() -> S + Unpin,
    B: Backoff,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        if let Some(delay) = this.delay.as_mut() {
            ready!(delay.as_mut().poll(cx));
            this.delay = None;
        }
        let watch = this.watch.get_or_insert_with(&mut this.start);
        let item = ready!(watch.poll_next_unpin(cx));
        match &item {
            Some(Err(error)) => {
                if forbidden(error) {
                    this.watch = None;
                }
                // The default backoff never runs out; one that did would retry without a delay.
                if let Some(delay) = this.backoff.next() {
                    this.delay = Some(Box::pin(tokio::time::sleep(delay)));
                }
            }
            Some(Ok(watcher::Event::Init)) | None => {}
            Some(Ok(_)) => this.backoff.reset(),
        }
        Poll::Ready(item)
    }
}

/// Starts one watch of `K` per namespace, or one on the whole cluster when `namespaces` is empty,
/// each with `config` and kube-runtime's default backoff (0.8 to 30 seconds), started over after
/// a `403` (`Restarting`). Each watch ends with a `(watch, None)` item, so the loop knows which
/// one ended.
fn watches<K>(client: &Client, namespaces: &[String], config: &watcher::Config) -> Watches<K>
where
    K: Resource<Scope = NamespaceResourceScope> + Clone + DeserializeOwned + Debug + Send + 'static,
    K::DynamicType: Default,
{
    let apis: Vec<(WatchKey, Api<K>)> = if namespaces.is_empty() {
        vec![(None, Api::all(client.clone()))]
    } else {
        namespaces
            .iter()
            .map(|n| (Some(n.clone()), Api::namespaced(client.clone(), n)))
            .collect()
    };
    stream::select_all(apis.into_iter().map(|(watch, api)| {
        let end = watch.clone();
        let config = config.clone();
        let start = move || watcher(api.clone(), config.clone()).boxed();
        Restarting::new(start, watcher::DefaultBackoff::default())
            .map(move |item| (watch.clone(), Some(item)))
            .chain(stream::once(async move { (end, None) }))
            .boxed()
    }))
}

/// How many explanations the deduplication cache keeps at most.
const DEDUP_CAPACITY: usize = 1024;

/// Why `kausa run` stopped with an error.
pub enum RunError {
    /// It could not start, or a watch ended (exit code 1).
    Failure(String),
    /// Its log could not be written (exit code 2).
    Output(io::Error),
}

impl From<io::Error> for RunError {
    fn from(error: io::Error) -> Self {
        RunError::Output(error)
    }
}

/// The client configuration: the in-cluster service account when kausa runs in a Pod (the API
/// server's address is in its environment), otherwise the current context of the kubeconfig. No
/// debug overrides from the environment (impersonation, another server address) are applied.
async fn client() -> Result<Client, String> {
    let config = if std::env::var_os("KUBERNETES_SERVICE_HOST").is_some() {
        Config::incluster().map_err(|e| e.to_string())?
    } else {
        Config::from_kubeconfig(&KubeConfigOptions::default())
            .await
            .map_err(|e| e.to_string())?
    };
    Client::try_from(config).map_err(|e| e.to_string())
}

/// The `namespaces` field of the `started` line: the watched namespaces sorted, so that an
/// operator can check the list, or `null` for the whole cluster.
fn watched(namespaces: &[String]) -> serde_json::Value {
    if namespaces.is_empty() {
        return serde_json::Value::Null;
    }
    let mut sorted = namespaces.to_vec();
    sorted.sort();
    json!(sorted)
}

/// The `notifications` field of the `started` line: the format and the webhook's host (never its
/// path, query or token), or `null` with notifications off.
fn notifications(notifier: Option<&Notifier>) -> serde_json::Value {
    notifier.map_or(
        serde_json::Value::Null,
        |n| json!({"format": n.format().name(), "host": n.host()}),
    )
}

/// The `leader_lease` field of the `started` line: the flag's own `<namespace>/<name>`, or `null`
/// without `--leader-lease`.
fn leader_lease(lease: Option<&LeaderLease>) -> serde_json::Value {
    lease.map_or(serde_json::Value::Null, |l| json!(l.flag()))
}

/// Runs until `SIGTERM` or `SIGINT`.
pub async fn run(options: Options) -> Result<(), RunError> {
    let client = client()
        .await
        .map_err(|e| RunError::Failure(format!("cannot configure the Kubernetes client: {e}")))?;
    let version = client
        .apiserver_version()
        .await
        .map_err(|e| RunError::Failure(format!("cannot reach the Kubernetes API server: {e}")))?;
    log::info(
        "started",
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "server": version.git_version,
            "settle_seconds": options.settle.as_secs(),
            "namespaces": watched(&options.namespaces),
            "rollouts": options.rollouts,
            "notifications": notifications(
                options.explain.as_ref().and_then(|s| s.notify.as_ref())
            ),
            "leader_lease": leader_lease(options.leader_lease.as_ref()),
        }),
    )?;

    let listen = |kind, name: &str| {
        signal(kind).map_err(|e| RunError::Failure(format!("cannot listen for {name}: {e}")))
    };
    let mut terminate = listen(SignalKind::terminate(), "SIGTERM")?;
    let mut interrupt = listen(SignalKind::interrupt(), "SIGINT")?;
    let metrics = Metrics::default();
    let listener = match &options.metrics_address {
        Some(address) => Some(
            TcpListener::bind(address)
                .await
                .map_err(|e| RunError::Failure(format!("cannot listen on {address}: {e}")))?,
        ),
        None => None,
    };
    // Unbounded without --leader-lease; closed while this replica is a standby
    // (docs/adr/0019-high-availability.md).
    let term = if options.leader_lease.is_some() {
        Term::closed()
    } else {
        Term::unbounded()
    };
    let shared = options.explain.as_ref().map(|settings| Shared {
        budget: RefCell::new(Budget::new(settings.calls_per_hour, settings.calls_per_day)),
        dedup: RefCell::new(Dedup::new(settings.dedup_ttl, DEDUP_CAPACITY)),
        writer: (settings.events && settings.model.is_some())
            .then(|| Writer::new(client.clone(), settings.instance.clone())),
        history: settings
            .history_days
            .filter(|_| settings.model.is_some())
            .map(|days| History::new(client.clone(), days)),
        metrics: &metrics,
        notifications: RefCell::new(VecDeque::new()),
        term: &term,
    });
    let lease = options
        .leader_lease
        .as_ref()
        .map(|l| KubeLease::new(client.clone(), &l.namespace, &l.name, TIMINGS.call_timeout));
    let cluster = KubeCluster { client };
    // A signal ends the process, so the stop is awaited at most once.
    let stop = async move {
        tokio::select! {
            biased;
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    };
    Serving {
        options: &options,
        lease: lease.as_ref(),
        cluster: &cluster,
        shared: shared.as_ref(),
        metrics: &metrics,
        term: &term,
        listener: listener.as_ref(),
        timings: TIMINGS,
        tick: TICK,
    }
    .serve(stop)
    .await
}

/// What a term acts on: its watches, its owner lookups and its explanations. `KubeCluster` in
/// production; the tests script it.
trait Cluster {
    type Lookup: OwnerLookup;

    /// Starts the watches of one term: Pods, Warning events and, with `rollouts`, Deployments, one
    /// of each per namespace or on the whole cluster (docs/adr/0013-namespaced-install-mode.md,
    /// docs/adr/0018-stuck-rollouts.md).
    fn watch(&self, namespaces: &[String], rollouts: bool) -> Watched;

    /// An owner lookup whose cache is empty, for one term.
    fn lookup(&self) -> Self::Lookup;

    /// The explanation of `job` (`explain::run`).
    fn explain<'a>(
        &'a self,
        job: Work,
        settings: &'a Settings,
        shared: &'a Shared<'_>,
    ) -> Explaining<'a>;
}

/// The watches of one term.
struct Watched {
    pods: Watches<Pod>,
    events: Watches<Event>,
    /// `None` with `--no-rollouts`.
    deployments: Option<Watches<Deployment>>,
}

/// The cluster through the API server.
struct KubeCluster {
    client: Client,
}

impl Cluster for KubeCluster {
    type Lookup = KubeLookup;

    fn watch(&self, namespaces: &[String], rollouts: bool) -> Watched {
        let selector = format!("type=Warning,reportingController!={REPORTING_CONTROLLER}");
        Watched {
            pods: watches::<Pod>(&self.client, namespaces, &watcher::Config::default()),
            events: watches::<Event>(
                &self.client,
                namespaces,
                &watcher::Config::default().fields(&selector),
            ),
            // Only Deployments past their progress deadline are kept, as records
            // (docs/adr/0018-stuck-rollouts.md).
            deployments: rollouts.then(|| {
                watches::<Deployment>(&self.client, namespaces, &watcher::Config::default())
            }),
        }
    }

    fn lookup(&self) -> KubeLookup {
        KubeLookup {
            client: self.client.clone(),
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn explain<'a>(
        &'a self,
        job: Work,
        settings: &'a Settings,
        shared: &'a Shared<'_>,
    ) -> Explaining<'a> {
        Box::pin(explain::run(job, self.client.clone(), settings, shared))
    }
}

/// Everything that lives across terms (docs/adr/0019-high-availability.md): the metrics and their
/// listener, the term, and `Shared` (the budget, the deduplication cache, the writers and the
/// notification queue). A term's own state lives in `lead`.
struct Serving<'a, L, C> {
    options: &'a Options,
    /// The Lease's API. kausa elects only with `options.leader_lease` too; without the flag, no
    /// Lease call is ever made.
    lease: Option<&'a L>,
    cluster: &'a C,
    shared: Option<&'a Shared<'a>>,
    metrics: &'a Metrics,
    term: &'a Term,
    listener: Option<&'a TcpListener>,
    timings: Timings,
    tick: Duration,
}

/// The election of this process: the flag, the Lease's API and the `lease error` log.
struct Election<'a, L> {
    flag: &'a LeaderLease,
    api: &'a L,
    /// The flag's own `<namespace>/<name>`, the only name of the Lease that is logged.
    name: String,
    errors: LeaseErrors,
}

/// Why a leading phase ended.
enum Ending {
    /// `SIGTERM` or `SIGINT`.
    Stop,
    /// The controller stopped on an error (a watch ended, the log could not be written).
    Ended(RunError),
    /// The Lease was lost; the process stays, as a standby.
    Lost(Lost),
}

/// What the unbiased arms of a leading phase gave.
enum Step {
    Ended(RunError),
    Accepted(io::Result<TcpStream>),
    Served,
}

/// The next metrics connection.
async fn accept(listener: Option<&TcpListener>) -> io::Result<TcpStream> {
    let listener = listener.expect("guarded by the condition");
    listener.accept().await.map(|(stream, _)| stream)
}

/// After a failed accept (for example, out of file descriptors): logs it and waits a little.
async fn accept_failed(error: &io::Error) -> io::Result<()> {
    log::warn("metrics accept error", json!({"error": error.to_string()}))?;
    tokio::time::sleep(ACCEPT_BACKOFF).await;
    Ok(())
}

impl<L: LeaseApi, C: Cluster> Serving<'_, L, C> {
    /// Serves the metrics and runs the controller until `stop`. Without `--leader-lease`, one
    /// leading phase that never ends but on `stop` or an error, as before. With it, a standby
    /// phase (the Lease read every retry period, and nothing else) and a leading phase in turn: a
    /// lost Lease drops the term and goes back to standby, never out of the process.
    ///
    /// Each phase is one `select!`, `biased` only for `stop` and then the election (the standby's
    /// reads, the leader's keeper), so that a clean stop wins and a term that ended steps down
    /// before the controller is polled again. The controller and the metrics arms share the rest
    /// at random, as in v0.3.0: a flood of metrics connections cannot starve the controller while
    /// the keeper renews.
    async fn serve(self, stop: impl Future<Output = ()>) -> Result<(), RunError> {
        let election = self
            .options
            .leader_lease
            .as_ref()
            .zip(self.lease)
            .map(|(flag, api)| {
                let name = flag.flag();
                Election {
                    errors: LeaseErrors::new(&name, LEASE_ERROR_PERIOD),
                    flag,
                    api,
                    name,
                }
            });
        if election.is_some() {
            // A standby until it holds the Lease: no model call, no write, gauge 0.
            self.term.close();
            self.metrics.set_leading(false);
        }
        let mut stop = pin!(stop);
        let mut connections = FuturesUnordered::new();
        'run: loop {
            let acquired = match &election {
                None => None,
                Some(election) => {
                    let mut standing = pin!(leader::standby(
                        election.api,
                        &election.flag.identity,
                        &self.timings,
                        self.metrics,
                        &election.errors,
                    ));
                    let acquired = loop {
                        tokio::select! {
                            biased;
                            () = &mut stop => break 'run,
                            acquired = &mut standing => break acquired?,
                            accepted = accept(self.listener),
                                if self.listener.is_some() && connections.len() < MAX_CONNECTIONS =>
                                match accepted {
                                    Ok(stream) => {
                                        connections.push(metrics::answer(stream, self.metrics));
                                    }
                                    Err(e) => accept_failed(&e).await?,
                                },
                            Some(()) = connections.next(), if !connections.is_empty() => {}
                        }
                    };
                    log::info("leading", json!({"lease": election.name}))?;
                    self.metrics
                        .inc("kausa_leadership_changes_total", &[("to", "leader")]);
                    self.metrics.set_leading(true);
                    Some(acquired)
                }
            };
            // The leading phase. Its block drops the controller, with everything in flight, and
            // the keeper before the term's end is handled.
            let ending = {
                let mut leading = pin!(lead(
                    self.options,
                    self.cluster,
                    self.shared,
                    self.metrics,
                    self.tick
                ));
                let mut holding = pin!(async {
                    match (acquired, &election) {
                        (Some(acquired), Some(election)) => {
                            leader::hold(
                                election.api,
                                &election.flag.identity,
                                acquired,
                                self.term,
                                &self.timings,
                                self.metrics,
                                &election.errors,
                            )
                            .await
                        }
                        // Without --leader-lease the term never ends.
                        _ => std::future::pending().await,
                    }
                });
                loop {
                    tokio::select! {
                        biased;
                        () = &mut stop => break Ending::Stop,
                        lost = &mut holding => break match lost {
                            Ok(lost) => Ending::Lost(lost),
                            Err(error) => Ending::Ended(error.into()),
                        },
                        // Not biased: the controller and the metrics arms share at random.
                        step = async {
                            tokio::select! {
                                result = &mut leading => {
                                    let Err(error) = result;
                                    Step::Ended(error)
                                }
                                accepted = accept(self.listener),
                                    if self.listener.is_some()
                                        && connections.len() < MAX_CONNECTIONS =>
                                    Step::Accepted(accepted),
                                Some(()) = connections.next(), if !connections.is_empty() => {
                                    Step::Served
                                }
                            }
                        } => match step {
                            Step::Ended(error) => break Ending::Ended(error),
                            Step::Accepted(Ok(stream)) => {
                                connections.push(metrics::answer(stream, self.metrics));
                            }
                            Step::Accepted(Err(e)) => accept_failed(&e).await?,
                            Step::Served => {}
                        },
                    }
                }
            };
            match ending {
                Ending::Stop => {
                    self.release(election.as_ref()).await?;
                    break 'run;
                }
                Ending::Ended(error) => {
                    // kausa exits with the error that ended the term (1, or 2 for its log), even
                    // if the release cannot be logged.
                    let _ = self.release(election.as_ref()).await;
                    return Err(error);
                }
                Ending::Lost(lost) => {
                    if let Some(election) = &election {
                        self.step_down(election, lost)?;
                    }
                }
            }
        }
        log::info("stopped", json!({}))?;
        Ok(())
    }

    /// Releases the Lease once the controller has stopped, while the term still holds: once, and
    /// the term is closed first, so nothing writes after it. Nothing without `--leader-lease`.
    async fn release(&self, election: Option<&Election<'_, L>>) -> io::Result<()> {
        let Some(election) = election else {
            return Ok(());
        };
        if !self.term.holds() {
            return Ok(());
        }
        let Some(version) = self.term.version() else {
            return Ok(());
        };
        self.term.close();
        leader::release(election.api, &version, &election.name, &self.timings).await
    }

    /// Steps down after a lost Lease, in place: the term is closed (the controller was already
    /// dropped with its records, queue, retries and owner cache), the notifications waiting for
    /// the webhook are dropped and counted, and the budget and the deduplication cache are kept
    /// for the next term.
    fn step_down(&self, election: &Election<'_, L>, lost: Lost) -> io::Result<()> {
        self.term.close();
        let reason = match lost {
            Lost::NotRenewed => "not renewed in time",
            Lost::HeldByAnother => "held by another",
        };
        log::warn(
            "leadership lost",
            json!({"lease": election.name, "reason": reason}),
        )?;
        self.metrics
            .inc("kausa_leadership_changes_total", &[("to", "standby")]);
        self.metrics.set_leading(false);
        if let Some(shared) = self.shared {
            let cleared = shared.notifications.borrow_mut().drain(..).count();
            if cleared > 0 {
                self.metrics.add(
                    "kausa_notifications_total",
                    &[("outcome", "dropped")],
                    cleared as u64,
                );
            }
        }
        Ok(())
    }
}

/// One term of the controller: the watches (Pods, Warning events and, unless `--no-rollouts`,
/// Deployments), the records, the tracker, the tick, the explanation queue and slot, the
/// notification slot, the delayed retries and a fresh owner lookup. All of it is dropped when the
/// term ends. Returns only on an error: a watch that ended, or a log that cannot be written.
async fn lead<C: Cluster>(
    options: &Options,
    cluster: &C,
    shared: Option<&Shared<'_>>,
    metrics: &Metrics,
    tick: Duration,
) -> Result<Infallible, RunError> {
    let lookup = cluster.lookup();
    let Watched {
        mut pods,
        mut events,
        mut deployments,
    } = cluster.watch(&options.namespaces, options.rollouts);
    let mut readiness = Readiness::new(&options.namespaces, options.rollouts);
    let mut state = State::default();
    let mut tracker = Tracker::new(options.settle);
    let mut ticker = tokio::time::interval(tick);
    let mut queue: VecDeque<Work> = VecDeque::new();
    let mut running: Option<Explaining<'_>> = None;
    // The notification being sent, beside the explanation slot: a slow webhook never holds it.
    let mut notifying: Option<Notifying<'_>> = None;
    // Jobs whose first call failed with a transient error, and when they may be tried again.
    let mut delayed: Vec<(Instant, Work)> = Vec::new();

    // When the last sweep of the history began (docs/adr/0014-explanation-history.md): a sweep is
    // due at once in each term.
    let mut last_sweep: Option<Instant> = None;

    loop {
        if let (Some(settings), Some(shared)) = (options.explain.as_ref(), shared) {
            let now = Instant::now();
            let history = shared.history.as_ref();
            match next_in_slot(
                running.is_some(),
                history.is_some(),
                last_sweep,
                now,
                !queue.is_empty(),
            ) {
                Some(Slot::Sweep) => {
                    if let Some(history) = history {
                        last_sweep = Some(now);
                        let namespaces = &options.namespaces;
                        running = Some(Box::pin(async move {
                            history
                                .sweep(namespaces, shared.metrics, shared.term)
                                .await
                                .map(|()| None)
                        }));
                    }
                }
                Some(Slot::Explain) => {
                    if let Some(job) = queue.pop_front() {
                        running = Some(cluster.explain(job, settings, shared));
                    }
                }
                None => {}
            }
            if let Some(notifier) = settings.notify.as_ref()
                && notifying.is_none()
            {
                let next = {
                    let dedup = shared.dedup.borrow();
                    notify::take_next(&mut shared.notifications.borrow_mut(), &dedup)
                };
                if let Some(item) = next {
                    notifying = Some(Box::pin(notify::deliver(notifier, item, shared)));
                }
            }
        }
        tokio::select! {
            result = async { running.as_mut().expect("guarded by the condition").await },
                if running.is_some() => {
                running = None;
                if let Some(job) = result? {
                    delayed.push((Instant::now() + explain::RETRY_DELAY, job));
                }
            }
            result = async { notifying.as_mut().expect("guarded by the condition").await },
                if notifying.is_some() => {
                notifying = None;
                // A failed send is only logged and counted; the error is kausa's own log.
                result?;
            }
            item = pods.next() => match item {
                Some((watch, Some(Ok(event)))) => {
                    let listed = matches!(event, watcher::Event::InitDone);
                    state.on_pod(&watch, event, &lookup).await?;
                    if listed && readiness.listed(Kind::Pods, &watch) {
                        metrics.set_ready();
                    }
                }
                Some((watch, Some(Err(e)))) => {
                    metrics.inc("kausa_watch_errors_total", &[("watch", "pods")]);
                    log::warn(
                        "pod watch error",
                        json!({"namespace": watch, "error": e.to_string()}),
                    )?;
                    state.on_watch_error(Kind::Pods, &watch, &e);
                }
                Some((watch, None)) => return Err(ended(Kind::Pods, &watch)),
                None => return Err(ended(Kind::Pods, &None)),
            },
            item = events.next() => match item {
                Some((watch, Some(Ok(event)))) => {
                    let listed = matches!(event, watcher::Event::InitDone);
                    state.on_event(&watch, event, &lookup).await?;
                    if listed && readiness.listed(Kind::Events, &watch) {
                        metrics.set_ready();
                    }
                }
                Some((watch, Some(Err(e)))) => {
                    metrics.inc("kausa_watch_errors_total", &[("watch", "events")]);
                    log::warn(
                        "event watch error",
                        json!({"namespace": watch, "error": e.to_string()}),
                    )?;
                    state.on_watch_error(Kind::Events, &watch, &e);
                }
                Some((watch, None)) => return Err(ended(Kind::Events, &watch)),
                None => return Err(ended(Kind::Events, &None)),
            },
            item = async { deployments.as_mut().expect("guarded by the condition").next().await },
                if deployments.is_some() => match item {
                Some((watch, Some(Ok(event)))) => {
                    let listed = matches!(event, watcher::Event::InitDone);
                    state.on_deployment(&watch, event);
                    if listed && readiness.listed(Kind::Deployments, &watch) {
                        metrics.set_ready();
                    }
                }
                Some((watch, Some(Err(e)))) => {
                    metrics.inc("kausa_watch_errors_total", &[("watch", "deployments")]);
                    log::warn(
                        "deployment watch error",
                        json!({"namespace": watch, "error": e.to_string()}),
                    )?;
                    state.on_watch_error(Kind::Deployments, &watch, &e);
                }
                Some((watch, None)) => return Err(ended(Kind::Deployments, &watch)),
                None => return Err(ended(Kind::Deployments, &None)),
            },
            _ = ticker.tick() => {
                let now = Timestamp::now();
                state.prune_events(now);
                let (ready, waiting): (Vec<_>, Vec<_>) =
                    delayed.drain(..).partition(|(at, _)| *at <= Instant::now());
                delayed = waiting;
                queue.extend(ready.into_iter().map(|(_, job)| job));
                for incident in state.tick(&mut tracker, now) {
                    metrics.inc("kausa_incidents_total", &[("detection", incident.key.detection.id())]);
                    log_incident(&incident, state.pods_of(&incident.owner))?;
                    if options.explain.is_some() {
                        if queue.len() >= QUEUE_LIMIT
                            && let Some(dropped) = queue.pop_front()
                        {
                            metrics.inc("kausa_explanations_dropped_total", &[]);
                            log::warn(
                                "explanation dropped",
                                json!({
                                    "fingerprint": dropped.gathered.incident.fingerprint,
                                    "reason": "the queue is full",
                                }),
                            )?;
                        }
                        queue.push_back(state.job(incident));
                    }
                }
            }
        }
    }
}

fn log_incident(incident: &Incident, pods_total: usize) -> io::Result<()> {
    let sample = &incident.sample;
    log::info(
        "incident",
        json!({
            "detection": incident.key.detection.id(),
            "detection_name": incident.key.detection.name(),
            "namespace": incident.key.namespace,
            "owner": {
                "apiVersion": incident.owner.api_version,
                "kind": incident.owner.kind,
                "name": incident.owner.name,
            },
            "pods": incident.pods,
            "pods_total": pods_total,
            "fingerprint": incident.fingerprint,
            "first_seen": incident.first_seen.to_string(),
            "sample": {
                "pod": sample.pod,
                "container": sample.container,
                "reason": sample.reason,
                "message": sample.message,
                "exit_code": sample.exit_code,
            },
        }),
    )
}

/// A Pod's record, its top-level owner, and whether that owner was resolved (a failed lookup
/// leaves the ReplicaSet or Job as a stand-in, retried on the Pod's next update).
type PodEntry = (PodRecord, OwnerRef, bool);

/// The records kausa keeps, keyed by UID.
#[derive(Default)]
struct State {
    pods: HashMap<String, PodEntry>,
    /// Only Warning events that a detection matches, with the top-level owner of the object an
    /// event is about when that is not a Pod (the Job's for `D008`, the controller's for `D011`,
    /// the claim or the autoscaler itself for `D012` and `D013`).
    events: HashMap<String, (EventRecord, Option<OwnerRef>)>,
    /// The UIDs seen during the open relist of each watch, to drop the ones that disappeared
    /// from that watch.
    pod_relist: HashMap<WatchKey, HashSet<String>>,
    event_relist: HashMap<WatchKey, HashSet<String>>,
    /// Only Deployments that `detect_rollout` matches (`D014`), by UID: a rollout that moves on,
    /// completes or is paused drops its record (docs/adr/0018-stuck-rollouts.md).
    deployments: HashMap<String, DeploymentRecord>,
    deployment_relist: HashMap<WatchKey, HashSet<String>>,
}

impl State {
    /// Applies an item of the Pod watch `watch`. A relist (`Init` to `InitDone`) drops only the
    /// Pods inside that watch that it did not see.
    async fn on_pod<L: OwnerLookup>(
        &mut self,
        watch: &WatchKey,
        event: watcher::Event<Pod>,
        lookup: &L,
    ) -> io::Result<()> {
        match event {
            watcher::Event::Init => {
                self.pod_relist.insert(watch.clone(), HashSet::new());
            }
            watcher::Event::InitDone => {
                if let Some(seen) = self.pod_relist.remove(watch) {
                    self.pods.retain(|uid, (record, _, _)| {
                        !inside(watch, &record.namespace) || seen.contains(uid)
                    });
                }
            }
            watcher::Event::Apply(pod) | watcher::Event::InitApply(pod) => {
                let Some(record) = PodRecord::from_pod(&pod) else {
                    return Ok(());
                };
                if let Some(seen) = self.pod_relist.get_mut(watch) {
                    seen.insert(record.uid.clone());
                }
                let (owner, resolved) = match self.pods.get(&record.uid) {
                    // The owner rarely changes: reuse it while the controller reference is the
                    // same and the lookup worked.
                    Some((old, owner, true)) if old.controller == record.controller => {
                        (owner.clone(), true)
                    }
                    _ => {
                        let itself = OwnerRef {
                            api_version: "v1".into(),
                            kind: "Pod".into(),
                            name: record.name.clone(),
                            uid: record.uid.clone(),
                        };
                        let controller = record.controller.as_ref();
                        let (owner, error) =
                            top_owner(lookup, &record.namespace, controller, &itself).await;
                        if let Some(error) = &error {
                            log::warn(
                                "owner lookup failed",
                                json!({
                                    "namespace": record.namespace,
                                    "pod": record.name,
                                    "error": error,
                                }),
                            )?;
                        }
                        (owner, error.is_none())
                    }
                };
                self.pods
                    .insert(record.uid.clone(), (record, owner, resolved));
            }
            watcher::Event::Delete(pod) => {
                if let Some(uid) = pod.metadata.uid {
                    self.pods.remove(&uid);
                }
            }
        }
        Ok(())
    }

    /// Applies an item of the event watch `watch`. A relist drops only the events inside that
    /// watch that it did not see; an event is inside by the namespace of the object it is about,
    /// which `EventRecord::from_event` makes equal to the Event's own namespace.
    async fn on_event<L: OwnerLookup>(
        &mut self,
        watch: &WatchKey,
        event: watcher::Event<Event>,
        lookup: &L,
    ) -> io::Result<()> {
        match event {
            watcher::Event::Init => {
                self.event_relist.insert(watch.clone(), HashSet::new());
            }
            watcher::Event::InitDone => {
                if let Some(seen) = self.event_relist.remove(watch) {
                    self.events.retain(|uid, (record, _)| {
                        !inside(watch, &record.regarding.namespace) || seen.contains(uid)
                    });
                }
            }
            watcher::Event::Apply(event) | watcher::Event::InitApply(event) => {
                let Some(record) = EventRecord::from_event(&event) else {
                    return Ok(());
                };
                if let Some(seen) = self.event_relist.get_mut(watch) {
                    seen.insert(record.uid.clone());
                }
                let now = Timestamp::now();
                let current = record
                    .last_seen
                    .is_some_and(|seen| now.duration_since(seen.min(now)) <= EVENT_FRESHNESS);
                let own = record.reporting_controller.as_deref() == Some(REPORTING_CONTROLLER);
                let detection = detect_event(&record);
                // Keep only what can be a signal: never kausa's own Events (in case a server does
                // not apply the field selector), and only recent events a detection matches.
                if own || detection.is_none() || !current {
                    self.events.remove(&record.uid);
                    return Ok(());
                }
                // An event about an object that is not a Pod is about that object's top-level
                // owner: a Job's CronJob, a ReplicaSet's Deployment, through the hops kausa may
                // follow (`owner.rs`); any other object is its own.
                let job_owner = if record.regarding.kind == "Pod" {
                    None
                } else {
                    let object = OwnerRef {
                        api_version: record.regarding.api_version.clone(),
                        kind: record.regarding.kind.clone(),
                        name: record.regarding.name.clone(),
                        uid: record.regarding.uid.clone().unwrap_or_default(),
                    };
                    let namespace = record.regarding.namespace.clone();
                    let (owner, error) =
                        top_owner(lookup, &namespace, Some(&object), &object).await;
                    if let Some(error) = error {
                        let field = object.kind.to_ascii_lowercase();
                        log::warn(
                            "owner lookup failed",
                            json!({"namespace": namespace, field: object.name, "error": error}),
                        )?;
                    }
                    Some(owner)
                };
                self.events.insert(record.uid.clone(), (record, job_owner));
            }
            watcher::Event::Delete(event) => {
                if let Some(uid) = event.metadata.uid {
                    self.events.remove(&uid);
                }
            }
        }
        Ok(())
    }

    /// Applies an item of the Deployment watch `watch`. Each Deployment is reduced to its record at
    /// once (invariant 8), kept only while `detect_rollout` matches it and removed otherwise; a
    /// relist drops only the records inside that watch that it did not see.
    fn on_deployment(&mut self, watch: &WatchKey, event: watcher::Event<Deployment>) {
        match event {
            watcher::Event::Init => {
                self.deployment_relist.insert(watch.clone(), HashSet::new());
            }
            watcher::Event::InitDone => {
                if let Some(seen) = self.deployment_relist.remove(watch) {
                    self.deployments.retain(|uid, record| {
                        !inside(watch, &record.namespace) || seen.contains(uid)
                    });
                }
            }
            watcher::Event::Apply(deployment) | watcher::Event::InitApply(deployment) => {
                let Some(record) = DeploymentRecord::from_deployment(&deployment) else {
                    return;
                };
                if let Some(seen) = self.deployment_relist.get_mut(watch) {
                    seen.insert(record.uid.clone());
                }
                if detect_rollout(&record) {
                    self.deployments.insert(record.uid.clone(), record);
                } else {
                    self.deployments.remove(&record.uid);
                }
            }
            watcher::Event::Delete(deployment) => {
                if let Some(uid) = deployment.metadata.uid {
                    self.deployments.remove(&uid);
                }
            }
        }
    }

    /// Forgets what a watch had shown when the API server refuses it (`403`): that kind's records
    /// inside the watch and its open relist, so that a revoked namespace stops giving signals.
    /// `Restarting` then starts a new watcher, whose relist brings them back once access is.
    /// Any other error keeps the records, and the watcher retries.
    fn on_watch_error(&mut self, kind: Kind, watch: &WatchKey, error: &watcher::Error) {
        if !forbidden(error) {
            return;
        }
        match kind {
            Kind::Pods => {
                self.pod_relist.remove(watch);
                self.pods
                    .retain(|_, (record, _, _)| !inside(watch, &record.namespace));
            }
            Kind::Events => {
                self.event_relist.remove(watch);
                self.events
                    .retain(|_, (record, _)| !inside(watch, &record.regarding.namespace));
            }
            Kind::Deployments => {
                self.deployment_relist.remove(watch);
                self.deployments
                    .retain(|_, record| !inside(watch, &record.namespace));
            }
        }
    }

    /// Drops events that can no longer be a signal, so the map holds only what happened in the
    /// last few minutes.
    fn prune_events(&mut self, now: Timestamp) {
        self.events.retain(|_, (record, _)| {
            record
                .last_seen
                .is_some_and(|seen| now.duration_since(seen.min(now)) <= EVENT_FRESHNESS)
        });
    }

    fn tick(&self, tracker: &mut Tracker, now: Timestamp) -> Vec<Incident> {
        let pods: Vec<_> = self
            .pods
            .values()
            .map(|(record, owner, _)| PodView { record, owner })
            .collect();
        let events: Vec<_> = self
            .events
            .values()
            .map(|(record, job_owner)| EventView {
                record,
                job_owner: job_owner.as_ref(),
            })
            .collect();
        let mut signals = collect_signals(&pods, &events, now, EVENT_FRESHNESS);
        let rollouts: Vec<_> = self.deployments.values().collect();
        signals.extend(rollout_signals(&rollouts, now));
        tracker.tick(now, signals)
    }

    /// What an explanation needs about an incident, copied out of the state: the incident's Pods
    /// (sample first), their events, which log to read and, for `D014`, the Deployment's record
    /// if kausa still keeps it.
    fn job(&self, incident: Incident) -> Work {
        let namespace = incident.key.namespace.clone();
        let sample = incident.sample.pod.clone();
        let mut pods: Vec<PodRecord> = self
            .pods
            .values()
            .filter(|(record, owner, _)| {
                record.namespace == namespace
                    && owner.uid == incident.owner.uid
                    && incident.pods.contains(&record.name)
            })
            .map(|(record, _, _)| record.clone())
            .collect();
        pods.sort_by_key(|p| (Some(&p.name) != sample.as_ref(), p.name.clone()));
        let mut events: Vec<EventRecord> = self
            .events
            .values()
            .filter(|(record, job_owner)| {
                record.regarding.namespace == namespace
                    && (incident.pods.contains(&record.regarding.name)
                        || job_owner
                            .as_ref()
                            .is_some_and(|o| o.uid == incident.owner.uid))
            })
            .map(|(record, _)| record.clone())
            .collect();
        events.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then(a.uid.cmp(&b.uid)));
        let log = self.log_source(&incident, &pods);
        let rollout = (incident.key.detection == Detection::RolloutStalled)
            .then(|| self.deployments.get(&incident.owner.uid).cloned())
            .flatten();
        Work {
            gathered: Gathered {
                pods_total: self.pods_of(&incident.owner),
                incident,
                pods,
                events,
                log_tail: None,
                rollout,
            },
            log,
            attempt: 0,
        }
    }

    /// The container whose log explains the incident, for the detections that read logs: the
    /// sample's container, or for a failed Job the failed container of one of its Pods.
    fn log_source(&self, incident: &Incident, pods: &[PodRecord]) -> Option<LogSource> {
        if !explain::reads_logs(incident.key.detection) {
            return None;
        }
        let candidates: Vec<&PodRecord> = if pods.is_empty() {
            let mut of_owner: Vec<&PodRecord> = self
                .pods
                .values()
                .filter(|(record, owner, _)| {
                    record.namespace == incident.key.namespace && owner.uid == incident.owner.uid
                })
                .map(|(record, _, _)| record)
                .collect();
            of_owner.sort_by(|a, b| a.name.cmp(&b.name));
            of_owner
        } else {
            pods.iter().collect()
        };
        candidates.into_iter().find_map(|pod| {
            let container = pod.containers.iter().find(|c| {
                incident.sample.container.as_deref() == Some(c.name.as_str())
                    || c.terminated_reason
                        .as_deref()
                        .is_some_and(|r| r != "Completed")
                    || c.last_termination_reason.is_some()
            })?;
            // The current instance runs or waits again after a crash: its log is the previous
            // instance's. A container still terminated holds its own log.
            let previous = container.restart_count > 0 && container.terminated_reason.is_none();
            Some(LogSource {
                namespace: pod.namespace.clone(),
                pod: pod.name.clone(),
                container: container.name.clone(),
                previous,
            })
        })
    }

    fn pods_of(&self, owner: &OwnerRef) -> usize {
        self.pods
            .values()
            .filter(|(_, o, _)| o.uid == owner.uid)
            .count()
    }
}

/// What holds the explanation slot: an explanation, which ends with a job to retry if its call
/// failed transiently, or a sweep of the history, which ends with none.
type Explaining<'a> = Pin<Box<dyn Future<Output = io::Result<Option<Work>>> + 'a>>;

/// What holds the notification slot: the delivery of one notification, with its retry and the
/// least interval before the next.
type Notifying<'a> = Pin<Box<dyn Future<Output = io::Result<()>> + 'a>>;

/// What takes the free explanation slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// A sweep of the history.
    Sweep,
    /// The next job of the queue.
    Explain,
}

/// Whether a sweep of the history is due: at start, and once `SWEEP_INTERVAL` has passed since the
/// last one began.
fn sweep_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= history::SWEEP_INTERVAL)
}

/// What takes the explanation slot: nothing while it is `busy`; with the history on, a due sweep
/// before the queue, so a queue that never empties cannot starve retention; otherwise the next job,
/// if one is `queued`.
fn next_in_slot(
    busy: bool,
    history: bool,
    last_sweep: Option<Instant>,
    now: Instant,
    queued: bool,
) -> Option<Slot> {
    if busy {
        None
    } else if history && sweep_due(last_sweep, now) {
        Some(Slot::Sweep)
    } else {
        queued.then_some(Slot::Explain)
    }
}

/// A cached owner lookup: the answer (or the error) and when it was read.
type CachedLookup = (Result<Option<OwnerRef>, String>, Instant);

/// Owner lookups against the API server: metadata only, with a timeout, cached for a while.
struct KubeLookup {
    client: Client,
    cache: RefCell<HashMap<(Hop, String, String), CachedLookup>>,
}

impl OwnerLookup for KubeLookup {
    async fn controller_of(
        &self,
        hop: Hop,
        namespace: &str,
        name: &str,
    ) -> Result<Option<OwnerRef>, String> {
        let fresh = |answer: &Result<Option<OwnerRef>, String>, at: &Instant| {
            let ttl = if answer.is_ok() {
                OWNER_CACHE_TTL
            } else {
                OWNER_ERROR_TTL
            };
            at.elapsed() < ttl
        };
        let key = (hop, namespace.to_owned(), name.to_owned());
        if let Some((answer, at)) = self.cache.borrow().get(&key)
            && fresh(answer, at)
        {
            return answer.clone();
        }
        let get = async {
            match hop {
                Hop::ReplicaSet => Api::<ReplicaSet>::namespaced(self.client.clone(), namespace)
                    .get_metadata(name)
                    .await
                    .map(|m| m.metadata),
                Hop::Job => Api::<Job>::namespaced(self.client.clone(), namespace)
                    .get_metadata(name)
                    .await
                    .map(|m| m.metadata),
            }
        };
        let answer = match tokio::time::timeout(OWNER_LOOKUP_TIMEOUT, get).await {
            Ok(Ok(metadata)) => Ok(controller_of(metadata.owner_references.as_ref())),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!(
                "no answer within {} seconds",
                OWNER_LOOKUP_TIMEOUT.as_secs()
            )),
        };
        let mut cache = self.cache.borrow_mut();
        cache.retain(|_, (answer, at)| fresh(answer, at));
        cache.insert(key, (answer.clone(), Instant::now()));
        answer
    }
}

// Tests of the namespaced mode (docs/adr/0013-namespaced-install-mode.md), without a cluster: the
// relists and refusals of each watch (`State::on_pod`, `State::on_event`,
// `State::on_watch_error`, `forbidden`), the new watcher after a refusal and its backoff
// (`Restarting`, over scripted watchers), readiness (`Readiness`) and the error of an ended watch
// (`ended`).
#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
    use kube::core::Status;

    /// Every Pod in these tests has no owner, so no lookup is made; a lookup is an error.
    struct NoLookup;

    impl OwnerLookup for NoLookup {
        async fn controller_of(
            &self,
            _hop: Hop,
            namespace: &str,
            name: &str,
        ) -> Result<Option<OwnerRef>, String> {
            Err(format!("unexpected owner lookup of {namespace}/{name}"))
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    fn ns(name: &str) -> WatchKey {
        Some(name.to_owned())
    }

    const CLUSTER: WatchKey = None;

    fn pod_uid(namespace: &str, name: &str) -> String {
        format!("pod-{namespace}-{name}")
    }

    fn event_uid(namespace: &str, name: &str) -> String {
        format!("event-{namespace}-{name}")
    }

    // Invented values only: no real cluster output (AGENTS.md, Security rules).
    fn pod(namespace: &str, name: &str) -> Pod {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": namespace, "uid": pod_uid(namespace, name)},
            "spec": {"containers": [{"name": "app", "image": "registry.example.com/app:1"}]},
            "status": {"phase": "Running"}
        }))
        .expect("the fixture is a valid Pod")
    }

    /// A fresh Warning Event that a detection matches (`D004`, a failing probe), so `State` keeps
    /// it.
    fn event(namespace: &str, name: &str) -> Event {
        let mut event: Event = serde_json::from_value(json!({
            "apiVersion": "events.k8s.io/v1",
            "kind": "Event",
            "metadata": {"name": format!("{name}.1"), "namespace": namespace,
                         "uid": event_uid(namespace, name)},
            "reason": "Unhealthy",
            "note": "Readiness probe failed: HTTP probe failed with statuscode: 503",
            "type": "Warning",
            "action": "Probe",
            "reportingController": "kubelet",
            "reportingInstance": "node-1",
            "regarding": {"apiVersion": "v1", "kind": "Pod", "namespace": namespace,
                          "name": name, "uid": pod_uid(namespace, name)}
        }))
        .expect("the fixture is a valid Event");
        event.event_time = Some(MicroTime(Timestamp::now()));
        event
    }

    fn on_pod(state: &mut State, watch: &WatchKey, event: watcher::Event<Pod>) {
        block_on(state.on_pod(watch, event, &NoLookup)).expect("the log is written");
    }

    fn on_event(state: &mut State, watch: &WatchKey, event: watcher::Event<Event>) {
        block_on(state.on_event(watch, event, &NoLookup)).expect("the log is written");
    }

    fn has_pod(state: &State, namespace: &str, name: &str) -> bool {
        state.pods.contains_key(&pod_uid(namespace, name))
    }

    fn has_event(state: &State, namespace: &str, name: &str) -> bool {
        state.events.contains_key(&event_uid(namespace, name))
    }

    /// A state with Pods `p1` and `p2` and Events about them in namespaces `a` and `b`, each
    /// applied by the watch of its namespace.
    fn two_namespaces() -> State {
        let mut state = State::default();
        for namespace in ["a", "b"] {
            for name in ["p1", "p2"] {
                on_pod(
                    &mut state,
                    &ns(namespace),
                    watcher::Event::Apply(pod(namespace, name)),
                );
                on_event(
                    &mut state,
                    &ns(namespace),
                    watcher::Event::Apply(event(namespace, name)),
                );
            }
        }
        assert_eq!((state.pods.len(), state.events.len()), (4, 4));
        state
    }

    /// The same records, applied by the cluster-wide watch.
    fn whole_cluster() -> State {
        let mut state = State::default();
        for namespace in ["a", "b"] {
            for name in ["p1", "p2"] {
                on_pod(
                    &mut state,
                    &CLUSTER,
                    watcher::Event::Apply(pod(namespace, name)),
                );
                on_event(
                    &mut state,
                    &CLUSTER,
                    watcher::Event::Apply(event(namespace, name)),
                );
            }
        }
        assert_eq!((state.pods.len(), state.events.len()), (4, 4));
        state
    }

    fn api_error(code: u16) -> kube::Error {
        kube::Error::Api(Box::new(
            Status::failure("an invented message", "an invented reason").with_code(code),
        ))
    }

    fn status(code: u16, reason: &str) -> Box<Status> {
        Box::new(Status::failure("an invented message", reason).with_code(code))
    }

    /// The three shapes in which kube reports a refused list or watch.
    fn forbidden_errors() -> Vec<watcher::Error> {
        vec![
            watcher::Error::InitialListFailed(api_error(403)),
            watcher::Error::WatchStartFailed(api_error(403)),
            watcher::Error::WatchError(status(403, "Forbidden")),
        ]
    }

    #[test]
    fn a_relist_of_one_namespace_drops_only_its_unseen_records() {
        let mut state = two_namespaces();
        on_pod(&mut state, &ns("a"), watcher::Event::Init);
        on_pod(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(pod("a", "p1")),
        );
        on_pod(&mut state, &ns("a"), watcher::Event::InitDone);
        assert!(has_pod(&state, "a", "p1"));
        assert!(!has_pod(&state, "a", "p2"), "unseen by a's relist");
        assert!(has_pod(&state, "b", "p1") && has_pod(&state, "b", "p2"));
        assert_eq!(state.events.len(), 4, "a Pod relist leaves the Events");

        on_event(&mut state, &ns("b"), watcher::Event::Init);
        on_event(
            &mut state,
            &ns("b"),
            watcher::Event::InitApply(event("b", "p2")),
        );
        on_event(&mut state, &ns("b"), watcher::Event::InitDone);
        assert!(!has_event(&state, "b", "p1"), "unseen by b's relist");
        assert!(has_event(&state, "b", "p2"));
        assert!(has_event(&state, "a", "p1") && has_event(&state, "a", "p2"));
        assert_eq!(state.pods.len(), 3, "an Event relist leaves the Pods");
    }

    #[test]
    fn relists_of_two_namespaces_at_once_do_not_mix() {
        let mut state = two_namespaces();
        // Both watches restart; b finishes first, while a's relist is still open.
        on_pod(&mut state, &ns("a"), watcher::Event::Init);
        on_pod(&mut state, &ns("b"), watcher::Event::Init);
        on_pod(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(pod("a", "p1")),
        );
        on_pod(
            &mut state,
            &ns("b"),
            watcher::Event::InitApply(pod("b", "p2")),
        );
        on_pod(&mut state, &ns("b"), watcher::Event::InitDone);
        assert!(has_pod(&state, "a", "p1") && has_pod(&state, "a", "p2"));
        assert!(!has_pod(&state, "b", "p1") && has_pod(&state, "b", "p2"));
        on_pod(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(pod("a", "p2")),
        );
        on_pod(&mut state, &ns("a"), watcher::Event::InitDone);
        assert!(has_pod(&state, "a", "p1") && has_pod(&state, "a", "p2"));
        assert_eq!(state.pods.len(), 3);

        on_event(&mut state, &ns("a"), watcher::Event::Init);
        on_event(&mut state, &ns("b"), watcher::Event::Init);
        on_event(
            &mut state,
            &ns("b"),
            watcher::Event::InitApply(event("b", "p1")),
        );
        on_event(&mut state, &ns("a"), watcher::Event::InitDone);
        assert!(!has_event(&state, "a", "p1") && !has_event(&state, "a", "p2"));
        assert!(has_event(&state, "b", "p1") && has_event(&state, "b", "p2"));
        on_event(&mut state, &ns("b"), watcher::Event::InitDone);
        assert!(has_event(&state, "b", "p1") && !has_event(&state, "b", "p2"));
    }

    #[test]
    fn a_forbidden_pod_watch_drops_only_the_pods_of_its_namespace() {
        for error in forbidden_errors() {
            assert!(forbidden(&error), "{error}");
            let mut state = two_namespaces();
            state.on_watch_error(Kind::Pods, &ns("a"), &error);
            assert!(
                !has_pod(&state, "a", "p1") && !has_pod(&state, "a", "p2"),
                "{error}"
            );
            assert!(
                has_pod(&state, "b", "p1") && has_pod(&state, "b", "p2"),
                "{error}"
            );
            assert_eq!(state.events.len(), 4, "{error}: the Events stay");
        }
    }

    #[test]
    fn a_forbidden_event_watch_drops_only_the_events_of_its_namespace() {
        for error in forbidden_errors() {
            let mut state = two_namespaces();
            state.on_watch_error(Kind::Events, &ns("b"), &error);
            assert!(
                !has_event(&state, "b", "p1") && !has_event(&state, "b", "p2"),
                "{error}"
            );
            assert!(
                has_event(&state, "a", "p1") && has_event(&state, "a", "p2"),
                "{error}"
            );
            assert_eq!(state.pods.len(), 4, "{error}: the Pods stay");
        }
    }

    /// An item of a watcher of Pods, as kube sends it.
    type PodItem = Result<watcher::Event<Pod>, watcher::Error>;

    /// What the loop sees of an item: its kind, and the Pod's name.
    fn label(item: &PodItem) -> String {
        let name = |pod: &Pod| pod.metadata.name.clone().unwrap_or_default();
        match item {
            Ok(watcher::Event::Init) => "Init".into(),
            Ok(watcher::Event::InitApply(pod)) => format!("InitApply {}", name(pod)),
            Ok(watcher::Event::InitDone) => "InitDone".into(),
            Ok(watcher::Event::Apply(pod)) => format!("Apply {}", name(pod)),
            Ok(watcher::Event::Delete(pod)) => format!("Delete {}", name(pod)),
            Err(error) if forbidden(error) => "forbidden".into(),
            Err(_) => "error".into(),
        }
    }

    /// A backoff with no delay that records how `Restarting` uses it.
    #[derive(Clone, Default)]
    struct Recorded(std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>);

    impl Recorded {
        fn calls(&self) -> Vec<&'static str> {
            self.0.lock().expect("not poisoned").clone()
        }
    }

    impl Iterator for Recorded {
        type Item = Duration;

        fn next(&mut self) -> Option<Duration> {
            self.0.lock().expect("not poisoned").push("next");
            Some(Duration::ZERO)
        }
    }

    impl Backoff for Recorded {
        fn reset(&mut self) {
            self.0.lock().expect("not poisoned").push("reset");
        }
    }

    /// Runs `Restarting` over scripted watchers, the first script for the first watcher and so
    /// on, until a watcher's script ends. Returns the items and how many watchers were started.
    fn restarting(scripts: Vec<Vec<PodItem>>, backoff: Recorded) -> (Vec<PodItem>, usize) {
        let scripts = RefCell::new(VecDeque::from(scripts));
        let started = std::cell::Cell::new(0);
        let start = || {
            started.set(started.get() + 1);
            stream::iter(scripts.borrow_mut().pop_front().unwrap_or_default())
        };
        let items = block_on(Restarting::new(start, backoff).collect::<Vec<_>>());
        (items, started.get())
    }

    #[test]
    fn a_forbidden_resumed_watch_lists_again_and_brings_back_its_pods() {
        // A tenant deletes kausa's RoleBinding in a: the open watch ends at its timeout and
        // resuming it is refused. kube-runtime would resume from its resource version once access
        // is back, sending only the Pods that changed (p3); Restarting starts a new watcher, which
        // lists a again.
        let scripts = vec![
            vec![
                Ok(watcher::Event::Init),
                Ok(watcher::Event::InitApply(pod("a", "p1"))),
                Ok(watcher::Event::InitApply(pod("a", "p2"))),
                Ok(watcher::Event::InitDone),
                Err(watcher::Error::WatchStartFailed(api_error(403))),
                Ok(watcher::Event::Apply(pod("a", "p3"))),
            ],
            vec![
                Ok(watcher::Event::Init),
                Ok(watcher::Event::InitApply(pod("a", "p1"))),
                Ok(watcher::Event::InitApply(pod("a", "p2"))),
                Ok(watcher::Event::InitDone),
            ],
        ];
        let (items, started) = restarting(scripts, Recorded::default());
        assert_eq!(started, 2);
        assert_eq!(
            items.iter().map(label).collect::<Vec<_>>(),
            [
                "Init",
                "InitApply p1",
                "InitApply p2",
                "InitDone",
                "forbidden",
                "Init",
                "InitApply p1",
                "InitApply p2",
                "InitDone",
            ]
        );
        let mut state = two_namespaces();
        for item in items {
            match item {
                Ok(event) => on_pod(&mut state, &ns("a"), event),
                Err(error) => {
                    state.on_watch_error(Kind::Pods, &ns("a"), &error);
                    assert!(!has_pod(&state, "a", "p1") && !has_pod(&state, "a", "p2"));
                    assert!(has_pod(&state, "b", "p1") && has_pod(&state, "b", "p2"));
                }
            }
        }
        assert!(has_pod(&state, "a", "p1") && has_pod(&state, "a", "p2"));
        assert!(!has_pod(&state, "a", "p3"), "the resumed watch was dropped");
        assert!(has_pod(&state, "b", "p1") && has_pod(&state, "b", "p2"));
    }

    #[test]
    fn every_shape_of_a_refusal_starts_a_new_watcher() {
        for error in forbidden_errors() {
            let text = error.to_string();
            let scripts = vec![
                vec![
                    Ok(watcher::Event::InitDone),
                    Err(error),
                    Ok(watcher::Event::Apply(pod("a", "p3"))),
                ],
                vec![Ok(watcher::Event::Init), Ok(watcher::Event::InitDone)],
            ];
            let (items, started) = restarting(scripts, Recorded::default());
            assert_eq!(started, 2, "{text}");
            assert_eq!(
                items.iter().map(label).collect::<Vec<_>>(),
                ["InitDone", "forbidden", "Init", "InitDone"],
                "{text}"
            );
        }
    }

    #[test]
    fn other_watch_errors_keep_the_watcher() {
        let errors = vec![
            watcher::Error::InitialListFailed(api_error(500)),
            watcher::Error::WatchStartFailed(api_error(401)),
            watcher::Error::WatchError(status(410, "Expired")),
            watcher::Error::WatchError(status(0, "Forbidden")),
            watcher::Error::NoResourceVersion,
        ];
        for error in errors {
            let text = error.to_string();
            let scripts = vec![
                vec![
                    Ok(watcher::Event::InitDone),
                    Err(error),
                    Ok(watcher::Event::Apply(pod("a", "p3"))),
                ],
                vec![Ok(watcher::Event::Init)],
            ];
            let backoff = Recorded::default();
            let (items, started) = restarting(scripts, backoff.clone());
            assert_eq!(started, 1, "{text}");
            assert_eq!(
                items.iter().map(label).collect::<Vec<_>>(),
                ["InitDone", "error", "Apply p3"],
                "{text}"
            );
            assert_eq!(backoff.calls(), ["reset", "next", "reset"], "{text}");
        }
    }

    #[test]
    fn a_namespace_that_keeps_refusing_backs_off_across_new_watchers() {
        // Every new watcher sends Init before its list is refused; Init must not reset the
        // delay, or a revoked namespace would be retried at the shortest delay forever.
        let refused = || {
            vec![
                Ok(watcher::Event::Init),
                Err(watcher::Error::InitialListFailed(api_error(403))),
            ]
        };
        let scripts = vec![
            refused(),
            refused(),
            refused(),
            vec![
                Ok(watcher::Event::Init),
                Ok(watcher::Event::InitApply(pod("a", "p1"))),
                Ok(watcher::Event::InitDone),
            ],
        ];
        let backoff = Recorded::default();
        let (items, started) = restarting(scripts, backoff.clone());
        assert_eq!(started, 4);
        assert_eq!(items.len(), 9);
        assert_eq!(
            backoff.calls(),
            ["next", "next", "next", "reset", "reset"],
            "one growing delay per refusal, reset only once the list worked"
        );
    }

    #[test]
    fn other_watch_errors_keep_the_records() {
        let errors = vec![
            watcher::Error::InitialListFailed(api_error(500)),
            watcher::Error::WatchStartFailed(api_error(401)),
            watcher::Error::WatchStartFailed(api_error(404)),
            watcher::Error::WatchError(status(410, "Expired")),
            watcher::Error::WatchError(status(500, "InternalError")),
            // The code decides, not the reason (Status::is_forbidden would match this one).
            watcher::Error::WatchError(status(0, "Forbidden")),
            watcher::Error::WatchFailed(kube::Error::LinesCodecMaxLineLengthExceeded),
            watcher::Error::NoResourceVersion,
        ];
        for error in errors {
            assert!(!forbidden(&error), "{error}");
            for kind in [Kind::Pods, Kind::Events] {
                for watch in [ns("a"), CLUSTER] {
                    let mut state = two_namespaces();
                    state.on_watch_error(kind, &watch, &error);
                    assert_eq!(
                        (state.pods.len(), state.events.len()),
                        (4, 4),
                        "{error} on {kind:?} {watch:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_forbidden_watch_leaves_the_open_relist_of_another_namespace() {
        let mut state = two_namespaces();
        on_pod(&mut state, &ns("a"), watcher::Event::Init);
        on_pod(&mut state, &ns("b"), watcher::Event::Init);
        on_pod(
            &mut state,
            &ns("b"),
            watcher::Event::InitApply(pod("b", "p1")),
        );
        state.on_watch_error(
            Kind::Pods,
            &ns("a"),
            &watcher::Error::InitialListFailed(api_error(403)),
        );
        on_pod(&mut state, &ns("b"), watcher::Event::InitDone);
        assert!(!has_pod(&state, "a", "p1") && !has_pod(&state, "a", "p2"));
        assert!(has_pod(&state, "b", "p1") && !has_pod(&state, "b", "p2"));

        on_event(&mut state, &ns("a"), watcher::Event::Init);
        on_event(&mut state, &ns("b"), watcher::Event::Init);
        on_event(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(event("a", "p2")),
        );
        state.on_watch_error(
            Kind::Events,
            &ns("b"),
            &watcher::Error::WatchError(status(403, "Forbidden")),
        );
        on_event(&mut state, &ns("a"), watcher::Event::InitDone);
        assert!(!has_event(&state, "b", "p1") && !has_event(&state, "b", "p2"));
        assert!(!has_event(&state, "a", "p1") && has_event(&state, "a", "p2"));
    }

    #[test]
    fn other_watch_errors_keep_the_open_relist() {
        let mut state = two_namespaces();
        on_pod(&mut state, &ns("a"), watcher::Event::Init);
        on_pod(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(pod("a", "p1")),
        );
        state.on_watch_error(
            Kind::Pods,
            &ns("a"),
            &watcher::Error::InitialListFailed(api_error(500)),
        );
        assert_eq!(state.pods.len(), 4, "the error keeps the records");
        on_pod(&mut state, &ns("a"), watcher::Event::InitDone);
        assert!(has_pod(&state, "a", "p1") && !has_pod(&state, "a", "p2"));

        on_event(&mut state, &ns("b"), watcher::Event::Init);
        on_event(
            &mut state,
            &ns("b"),
            watcher::Event::InitApply(event("b", "p1")),
        );
        state.on_watch_error(Kind::Events, &ns("b"), &watcher::Error::NoResourceVersion);
        assert_eq!(state.events.len(), 4, "the error keeps the records");
        on_event(&mut state, &ns("b"), watcher::Event::InitDone);
        assert!(has_event(&state, "b", "p1") && !has_event(&state, "b", "p2"));
    }

    #[test]
    fn no_watch_keeps_an_event_about_an_object_in_another_namespace() {
        // An Event in a about an object in elsewhere (docs/adr/0013-namespaced-install-mode.md):
        // EventRecord::from_event drops it, so no watch keeps it and no relist or refusal of
        // elsewhere could reach into a.
        let mut crossed = event("a", "p3");
        crossed
            .regarding
            .as_mut()
            .expect("the fixture has a regarding object")
            .namespace = Some("elsewhere".to_owned());
        for watch in [ns("a"), ns("elsewhere"), CLUSTER] {
            let mut state = two_namespaces();
            on_event(&mut state, &watch, watcher::Event::Apply(crossed.clone()));
            on_event(
                &mut state,
                &watch,
                watcher::Event::InitApply(crossed.clone()),
            );
            assert!(!has_event(&state, "a", "p3"), "{watch:?}");
            assert_eq!(state.events.len(), 4, "{watch:?}");
        }
    }

    #[test]
    fn the_cluster_wide_watch_relists_as_in_v0_1() {
        let mut state = whole_cluster();
        on_pod(&mut state, &CLUSTER, watcher::Event::Init);
        on_pod(
            &mut state,
            &CLUSTER,
            watcher::Event::InitApply(pod("a", "p1")),
        );
        on_pod(&mut state, &CLUSTER, watcher::Event::InitDone);
        assert!(has_pod(&state, "a", "p1"));
        assert_eq!(state.pods.len(), 1, "every unseen Pod is dropped");
        assert_eq!(state.events.len(), 4);

        on_event(&mut state, &CLUSTER, watcher::Event::Init);
        on_event(
            &mut state,
            &CLUSTER,
            watcher::Event::InitApply(event("b", "p2")),
        );
        on_event(&mut state, &CLUSTER, watcher::Event::InitDone);
        assert!(has_event(&state, "b", "p2"));
        assert_eq!(state.events.len(), 1, "every unseen Event is dropped");
        assert_eq!(state.pods.len(), 1);
    }

    #[test]
    fn a_forbidden_cluster_wide_watch_drops_every_record_of_its_kind() {
        for error in forbidden_errors() {
            let mut state = whole_cluster();
            state.on_watch_error(Kind::Pods, &CLUSTER, &error);
            assert_eq!(state.pods.len(), 0, "{error}");
            assert_eq!(state.events.len(), 4, "{error}");
            state.on_watch_error(Kind::Events, &CLUSTER, &error);
            assert_eq!(state.events.len(), 0, "{error}");
        }
    }

    #[test]
    fn deletes_still_remove_a_record_whatever_the_watch() {
        let mut state = two_namespaces();
        on_pod(&mut state, &ns("a"), watcher::Event::Delete(pod("a", "p1")));
        on_event(
            &mut state,
            &ns("b"),
            watcher::Event::Delete(event("b", "p1")),
        );
        assert!(!has_pod(&state, "a", "p1") && !has_event(&state, "b", "p1"));
        assert_eq!((state.pods.len(), state.events.len()), (3, 3));
    }

    #[test]
    fn readiness_waits_for_every_watch_of_every_namespace() {
        let namespaces = vec!["a".to_owned(), "b".to_owned()];
        let mut readiness = Readiness::new(&namespaces, false);
        assert!(!readiness.listed(Kind::Pods, &ns("a")));
        assert!(
            !readiness.listed(Kind::Pods, &ns("a")),
            "a repeated InitDone of one watch counts once"
        );
        assert!(!readiness.listed(Kind::Events, &ns("a")));
        assert!(!readiness.listed(Kind::Pods, &ns("b")));
        assert!(!readiness.listed(Kind::Pods, &ns("b")));
        assert!(
            readiness.listed(Kind::Events, &ns("b")),
            "ready once all 2 x 2 watches have listed"
        );
        // A later 403 drops records but not readiness: it never goes back.
        let mut state = two_namespaces();
        state.on_watch_error(
            Kind::Pods,
            &ns("a"),
            &watcher::Error::InitialListFailed(api_error(403)),
        );
        assert!(readiness.listed(Kind::Pods, &ns("a")));
        assert!(readiness.listed(Kind::Events, &ns("b")));
    }

    #[test]
    fn readiness_in_cluster_mode_waits_for_both_watches() {
        let mut readiness = Readiness::new(&[], false);
        assert!(!readiness.listed(Kind::Events, &CLUSTER));
        assert!(!readiness.listed(Kind::Events, &CLUSTER));
        assert!(readiness.listed(Kind::Pods, &CLUSTER));
        assert!(readiness.listed(Kind::Pods, &CLUSTER));
    }

    #[test]
    fn readiness_with_one_namespace_needs_its_two_watches() {
        let mut readiness = Readiness::new(&["shop".to_owned()], false);
        assert!(!readiness.listed(Kind::Pods, &ns("shop")));
        assert!(readiness.listed(Kind::Events, &ns("shop")));
    }

    #[test]
    fn an_ended_watch_is_a_failure_that_names_it() {
        let failure = |kind, watch: &WatchKey| match ended(kind, watch) {
            RunError::Failure(message) => message,
            RunError::Output(error) => panic!("not an output error: {error}"),
        };
        assert_eq!(
            failure(Kind::Pods, &ns("shop")),
            "the pod watch of namespace shop ended"
        );
        assert_eq!(
            failure(Kind::Events, &ns("shop")),
            "the event watch of namespace shop ended"
        );
        assert_eq!(failure(Kind::Pods, &CLUSTER), "the pod watch ended");
        assert_eq!(failure(Kind::Events, &CLUSTER), "the event watch ended");
    }

    #[test]
    fn started_names_the_watched_namespaces_sorted_or_null() {
        assert_eq!(watched(&[]), serde_json::Value::Null);
        let given = ["web".to_owned(), "api".to_owned(), "shop".to_owned()];
        assert_eq!(watched(&given), json!(["api", "shop", "web"]));
        assert_eq!(given, ["web", "api", "shop"]);
    }

    #[test]
    fn started_names_the_notification_format_and_host_only() {
        use crate::notification::ChatId;
        use crate::notify::Format;

        assert_eq!(notifications(None), serde_json::Value::Null);
        // A visibly fake token, built to look like a Telegram bot token.
        let token = format!("123456789:{}", "FAKE-token-for-tests-only-000000000");
        let text = format!("https://api.example.com:8443/bot{token}/sendMessage?x=1");
        let format = Format::Telegram(ChatId::Id(-1001234567890));
        let url = notify::check_url(&text, &format, false).expect("a valid URL");
        let notifier = Notifier::new(url, format, None, false).expect("a client");
        let field = notifications(Some(&notifier));
        assert_eq!(
            field,
            json!({"format": "telegram", "host": "api.example.com"})
        );
        assert!(!field.to_string().contains(&token));

        let url = notify::check_url("https://hooks.example.com/a/b", &Format::Json, false)
            .expect("a valid URL");
        let bearer = Some("FAKE-bearer-for-tests-only".to_owned());
        let notifier = Notifier::new(url, Format::Json, bearer, false).expect("a client");
        let field = notifications(Some(&notifier));
        assert_eq!(
            field,
            json!({"format": "json", "host": "hooks.example.com"})
        );
        assert!(!field.to_string().contains("FAKE-bearer"));
    }

    // Acceptance tests for the history sweep (docs/adr/0014-explanation-history.md), written
    // before the code. They use this API, which run.rs does not have yet:
    //
    // - `fn sweep_due(last: Option<Instant>, now: Instant) -> bool`: true with no sweep yet, and
    //   once `history::SWEEP_INTERVAL` has passed since the last one began;
    // - `enum Slot { Sweep, Explain }` (`Clone`, `Copy`, `Debug`, `PartialEq`, `Eq`);
    // - `fn next_in_slot(busy: bool, history: bool, last_sweep: Option<Instant>, now: Instant,
    //   queued: bool) -> Option<Slot>`: what takes the explanation slot. Nothing while it is
    //   busy; with the history on, a due sweep before the queue; otherwise the next job, if any.

    #[test]
    fn a_sweep_is_due_at_start_and_once_the_interval_has_passed() {
        let start = Instant::now();
        let interval = crate::history::SWEEP_INTERVAL;
        assert!(sweep_due(None, start));
        assert!(!sweep_due(Some(start), start));
        assert!(!sweep_due(
            Some(start),
            start + interval - Duration::from_secs(1)
        ));
        assert!(sweep_due(Some(start), start + interval));
        assert!(sweep_due(Some(start), start + interval * 3));
    }

    #[test]
    fn a_due_sweep_takes_the_slot_before_a_waiting_job() {
        let start = Instant::now();
        let later = start + Duration::from_secs(60);
        // Due (never swept, or an hour ago): the sweep goes first, queue or not.
        assert_eq!(
            next_in_slot(false, true, None, start, true),
            Some(Slot::Sweep)
        );
        assert_eq!(
            next_in_slot(false, true, None, start, false),
            Some(Slot::Sweep)
        );
        let hour_ago = start;
        let now = start + crate::history::SWEEP_INTERVAL;
        assert_eq!(
            next_in_slot(false, true, Some(hour_ago), now, true),
            Some(Slot::Sweep)
        );
        // Not due: the queue, or nothing.
        assert_eq!(
            next_in_slot(false, true, Some(start), later, true),
            Some(Slot::Explain)
        );
        assert_eq!(next_in_slot(false, true, Some(start), later, false), None);
    }

    #[test]
    fn nothing_starts_while_the_slot_is_busy() {
        let start = Instant::now();
        for history in [true, false] {
            for last in [None, Some(start)] {
                for queued in [true, false] {
                    let now = start + crate::history::SWEEP_INTERVAL * 2;
                    assert_eq!(
                        next_in_slot(true, history, last, now, queued),
                        None,
                        "history {history}, last {last:?}, queued {queued}"
                    );
                }
            }
        }
    }

    #[test]
    fn there_is_no_sweep_without_the_history() {
        let start = Instant::now();
        assert_eq!(
            next_in_slot(false, false, None, start, true),
            Some(Slot::Explain)
        );
        assert_eq!(next_in_slot(false, false, None, start, false), None);
        let now = start + crate::history::SWEEP_INTERVAL * 5;
        assert_eq!(
            next_in_slot(false, false, Some(start), now, true),
            Some(Slot::Explain)
        );
        assert_eq!(next_in_slot(false, false, Some(start), now, false), None);
    }

    #[test]
    fn a_queue_that_never_empties_cannot_starve_the_sweep() {
        // Three hours of a slot that is never idle: each explanation holds it for 45 seconds and
        // the queue always has another; a sweep holds it for SWEEP_TIME. The loop records when a
        // sweep starts, as run does.
        let interval = crate::history::SWEEP_INTERVAL;
        let explanation = Duration::from_secs(45);
        let start = Instant::now();
        let mut elapsed = Duration::ZERO;
        let mut last_sweep: Option<Instant> = None;
        let mut sweeps: Vec<Duration> = Vec::new();
        let mut explained = 0;
        while elapsed < interval * 3 + Duration::from_secs(120) {
            let now = start + elapsed;
            match next_in_slot(false, true, last_sweep, now, true) {
                Some(Slot::Sweep) => {
                    sweeps.push(elapsed);
                    last_sweep = Some(now);
                    elapsed += crate::history::SWEEP_TIME;
                }
                Some(Slot::Explain) => {
                    explained += 1;
                    elapsed += explanation;
                }
                None => panic!("the slot stayed idle with a job waiting at {elapsed:?}"),
            }
        }
        assert_eq!(sweeps.first(), Some(&Duration::ZERO), "{sweeps:?}");
        assert!(sweeps.len() >= 4, "{sweeps:?}");
        for pair in sweeps.windows(2) {
            let gap = pair[1] - pair[0];
            assert!(
                gap >= interval && gap < interval + explanation,
                "{sweeps:?}"
            );
        }
        assert!(
            explained > 200,
            "the sweep took the slot from the queue: {explained}"
        );
    }

    #[test]
    fn watch_error_lines_name_the_namespace_or_null() {
        let field = |watch: &WatchKey| json!({"namespace": watch})["namespace"].clone();
        assert_eq!(field(&ns("shop")), json!("shop"));
        assert_eq!(field(&CLUSTER), serde_json::Value::Null);
    }

    // Acceptance tests for `D011`, `D012` and `D013`, written before the code: what `State` keeps
    // of an event about a workload controller, a claim or an autoscaler, and what it gathers for
    // the explanation. Only the two hops kausa may already follow are looked up (a ReplicaSet's
    // Deployment, a Job's CronJob); every other object is its own top-level owner, so the rules
    // need no new permission.

    /// Answers the two lookups kausa may make; any other lookup is an error.
    struct Parents;

    impl OwnerLookup for Parents {
        async fn controller_of(
            &self,
            hop: Hop,
            namespace: &str,
            name: &str,
        ) -> Result<Option<OwnerRef>, String> {
            let parent = |api_version: &str, kind: &str, name: &str| OwnerRef {
                api_version: api_version.into(),
                kind: kind.into(),
                name: name.into(),
                uid: format!("uid-{name}"),
            };
            match (hop, namespace, name) {
                (Hop::ReplicaSet, "shop", "checkout-7fffb9bf5c") => {
                    Ok(Some(parent("apps/v1", "Deployment", "checkout")))
                }
                (Hop::Job, "shop", "report-29000000") => {
                    Ok(Some(parent("batch/v1", "CronJob", "report")))
                }
                _ => Err(format!("unexpected owner lookup of {namespace}/{name}")),
            }
        }
    }

    /// A Warning Event about the object `(api_version, kind, name)` in `shop`, repeated for the
    /// last five minutes.
    fn event_about((api_version, kind, name): (&str, &str, &str), reason: &str) -> Event {
        let mut event: Event = serde_json::from_value(json!({
            "apiVersion": "events.k8s.io/v1",
            "kind": "Event",
            "metadata": {"name": format!("{name}.1"), "namespace": "shop",
                         "uid": format!("event-{name}")},
            "reason": reason,
            "note": "an invented note",
            "type": "Warning",
            "action": "Create",
            "reportingController": "example-controller",
            "reportingInstance": "example-controller-1",
            "regarding": {"apiVersion": api_version, "kind": kind, "namespace": "shop",
                          "name": name, "uid": format!("uid-{name}")}
        }))
        .expect("the fixture is a valid Event");
        let now = Timestamp::now();
        let first = now
            .checked_sub(SignedDuration::from_mins(5))
            .expect("a valid time");
        event.event_time = Some(MicroTime(first));
        event.series = Some(k8s_openapi::api::events::v1::EventSeries {
            count: 12,
            last_observed_time: MicroTime(now),
        });
        event
    }

    #[test]
    fn events_about_controllers_claims_and_autoscalers_are_incidents_about_their_owner() {
        let cases = [
            (
                ("apps/v1", "ReplicaSet", "checkout-7fffb9bf5c"),
                "FailedCreate",
                "D011",
                ("Deployment", "checkout"),
            ),
            (
                ("batch/v1", "Job", "report-29000000"),
                "FailedCreate",
                "D011",
                ("CronJob", "report"),
            ),
            (
                ("apps/v1", "StatefulSet", "db"),
                "FailedCreate",
                "D011",
                ("StatefulSet", "db"),
            ),
            (
                ("apps/v1", "DaemonSet", "node-agent"),
                "FailedCreate",
                "D011",
                ("DaemonSet", "node-agent"),
            ),
            (
                ("v1", "PersistentVolumeClaim", "orders-data"),
                "ProvisioningFailed",
                "D012",
                ("PersistentVolumeClaim", "orders-data"),
            ),
            (
                ("autoscaling/v2", "HorizontalPodAutoscaler", "checkout"),
                "FailedGetScale",
                "D013",
                ("HorizontalPodAutoscaler", "checkout"),
            ),
        ];
        for (about, reason, id, (kind, name)) in cases {
            let mut state = State::default();
            let event = event_about(about, reason);
            block_on(state.on_event(&CLUSTER, watcher::Event::Apply(event), &Parents))
                .expect("the log is written");
            let mut tracker = Tracker::new(SignedDuration::from_secs(60));
            let incidents = state.tick(&mut tracker, Timestamp::now());
            assert_eq!(incidents.len(), 1, "{about:?}");
            let incident = incidents.into_iter().next().expect("an incident");
            assert_eq!(incident.key.detection.id(), id, "{about:?}");
            assert_eq!(
                (incident.owner.kind.as_str(), incident.owner.name.as_str()),
                (kind, name),
                "{about:?}"
            );
            assert_eq!(incident.owner.uid, format!("uid-{name}"), "{about:?}");
            // The explanation gathers the event, no Pod, and reads no log.
            let work = state.job(incident);
            let events: Vec<_> = work.gathered.events.iter().map(|e| &e.uid).collect();
            assert_eq!(events, [&format!("event-{}", about.2)], "{about:?}");
            assert!(work.gathered.pods.is_empty(), "{about:?}");
            assert!(work.log.is_none(), "{about:?}");
        }
    }

    /// Records every owner lookup, and answers it as `Parents` does.
    #[derive(Default)]
    struct Lookups(std::cell::RefCell<Vec<(Hop, String)>>);

    impl OwnerLookup for Lookups {
        async fn controller_of(
            &self,
            hop: Hop,
            namespace: &str,
            name: &str,
        ) -> Result<Option<OwnerRef>, String> {
            self.0.borrow_mut().push((hop, name.to_owned()));
            Parents.controller_of(hop, namespace, name).await
        }
    }

    #[test]
    fn only_replicasets_and_jobs_are_looked_up_and_a_failed_lookup_keeps_the_controller() {
        // `D011` to `D013` need no new permission: kausa may `get` ReplicaSets and Jobs only, so
        // an event about a StatefulSet, a DaemonSet, a claim or an autoscaler, or a Deployment's
        // own event, makes no lookup at all.
        let lookups = Lookups::default();
        let mut state = State::default();
        for (about, reason) in [
            (("apps/v1", "StatefulSet", "db"), "FailedCreate"),
            (("apps/v1", "DaemonSet", "node-agent"), "FailedCreate"),
            (
                ("v1", "PersistentVolumeClaim", "orders-data"),
                "ProvisioningFailed",
            ),
            (
                ("autoscaling/v2", "HorizontalPodAutoscaler", "checkout"),
                "FailedGetScale",
            ),
            (("apps/v1", "Deployment", "storefront"), "FailedCreate"),
            (
                ("apps/v1", "ReplicaSet", "checkout-7fffb9bf5c"),
                "FailedDelete",
            ),
        ] {
            block_on(state.on_event(
                &CLUSTER,
                watcher::Event::Apply(event_about(about, reason)),
                &lookups,
            ))
            .expect("the log is written");
        }
        assert!(lookups.0.borrow().is_empty(), "{:?}", lookups.0.borrow());
        // The two events no detection matches are not kept.
        assert_eq!(state.events.len(), 4);
        // An event about a ReplicaSet is looked up; when the lookup fails, its incident names the
        // ReplicaSet itself until a lookup works (docs/reference/command-line.md).
        let orphan = ("apps/v1", "ReplicaSet", "orphan-5d4c3b2a1");
        let mut state = State::default();
        block_on(state.on_event(
            &CLUSTER,
            watcher::Event::Apply(event_about(orphan, "FailedCreate")),
            &lookups,
        ))
        .expect("the log is written");
        assert_eq!(
            *lookups.0.borrow(),
            [(Hop::ReplicaSet, "orphan-5d4c3b2a1".to_owned())]
        );
        let mut tracker = Tracker::new(SignedDuration::from_secs(60));
        let incidents = state.tick(&mut tracker, Timestamp::now());
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        let incident = &incidents[0];
        assert_eq!(incident.key.detection.id(), "D011");
        assert_eq!(
            (incident.owner.kind.as_str(), incident.owner.name.as_str()),
            ("ReplicaSet", "orphan-5d4c3b2a1")
        );
        assert_eq!(incident.owner.uid, "uid-orphan-5d4c3b2a1");
    }

    // Acceptance tests for `D014` (docs/adr/0018-stuck-rollouts.md), written before the code: the
    // Deployment watch, on unless `--no-rollouts`. `State` keeps a Deployment's record only while
    // `detect_rollout` matches it, by UID; relists and `403`s work per watch as for Pods and
    // events (docs/adr/0013-namespaced-install-mode.md), and readiness counts the third watch.

    fn deployment_uid(namespace: &str, name: &str) -> String {
        format!("deployment-{namespace}-{name}")
    }

    /// A Deployment past its progress deadline (`stalled`), or one whose rollout completed.
    // Invented values only: no real cluster output (AGENTS.md, Security rules).
    fn deployment(
        namespace: &str,
        name: &str,
        stalled: bool,
    ) -> k8s_openapi::api::apps::v1::Deployment {
        let (status, reason) = if stalled {
            ("False", "ProgressDeadlineExceeded")
        } else {
            ("True", "NewReplicaSetAvailable")
        };
        serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": name, "namespace": namespace,
                         "uid": deployment_uid(namespace, name), "generation": 2,
                         "annotations": {"deployment.kubernetes.io/revision": "3"}},
            "spec": {
                "replicas": 2,
                "progressDeadlineSeconds": 20,
                "selector": {"matchLabels": {"app": name}},
                "template": {
                    "metadata": {"labels": {"app": name}},
                    "spec": {"containers": [{"name": "app",
                                             "image": "registry.example.com/app:1"}]}
                }
            },
            "status": {
                "observedGeneration": 2,
                "replicas": 2,
                "updatedReplicas": 1,
                "readyReplicas": 1,
                "availableReplicas": 1,
                "unavailableReplicas": 1,
                "conditions": [{"type": "Progressing", "status": status, "reason": reason}]
            }
        }))
        .expect("the fixture is a valid Deployment")
    }

    fn on_deployment(
        state: &mut State,
        watch: &WatchKey,
        event: watcher::Event<k8s_openapi::api::apps::v1::Deployment>,
    ) {
        state.on_deployment(watch, event);
    }

    fn has_deployment(state: &State, namespace: &str, name: &str) -> bool {
        state
            .deployments
            .contains_key(&deployment_uid(namespace, name))
    }

    /// `two_namespaces`, plus stalled Deployments `d1` and `d2` in `a` and `b`, each applied by
    /// the watch of its namespace.
    fn two_namespaces_with_rollouts() -> State {
        let mut state = two_namespaces();
        for namespace in ["a", "b"] {
            for name in ["d1", "d2"] {
                on_deployment(
                    &mut state,
                    &ns(namespace),
                    watcher::Event::Apply(deployment(namespace, name, true)),
                );
            }
        }
        assert_eq!(state.deployments.len(), 4);
        state
    }

    /// The same records, applied by the cluster-wide watches.
    fn whole_cluster_with_rollouts() -> State {
        let mut state = whole_cluster();
        for namespace in ["a", "b"] {
            for name in ["d1", "d2"] {
                on_deployment(
                    &mut state,
                    &CLUSTER,
                    watcher::Event::Apply(deployment(namespace, name, true)),
                );
            }
        }
        assert_eq!(state.deployments.len(), 4);
        state
    }

    #[test]
    fn a_stalled_deployment_is_kept_as_its_record_by_uid() {
        let mut state = State::default();
        let stalled = deployment("shop", "checkout", true);
        on_deployment(&mut state, &CLUSTER, watcher::Event::Apply(stalled.clone()));
        let expected = crate::record::DeploymentRecord::from_deployment(&stalled)
            .expect("a complete Deployment");
        assert_eq!(
            state.deployments.get(&deployment_uid("shop", "checkout")),
            Some(&expected)
        );
        assert_eq!(state.deployments.len(), 1);
    }

    #[test]
    fn a_deployment_relist_of_one_namespace_drops_only_its_unseen_records() {
        let mut state = two_namespaces_with_rollouts();
        on_deployment(&mut state, &ns("a"), watcher::Event::Init);
        on_deployment(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(deployment("a", "d1", true)),
        );
        on_deployment(&mut state, &ns("a"), watcher::Event::InitDone);
        assert!(has_deployment(&state, "a", "d1"));
        assert!(!has_deployment(&state, "a", "d2"), "unseen by a's relist");
        assert!(has_deployment(&state, "b", "d1") && has_deployment(&state, "b", "d2"));
        assert_eq!(
            (state.pods.len(), state.events.len()),
            (4, 4),
            "a Deployment relist leaves the Pods and the Events"
        );
        // And a Pod relist leaves the Deployments.
        on_pod(&mut state, &ns("b"), watcher::Event::Init);
        on_pod(&mut state, &ns("b"), watcher::Event::InitDone);
        assert_eq!(state.deployments.len(), 3);
    }

    #[test]
    fn deployment_relists_of_two_namespaces_at_once_do_not_mix() {
        let mut state = two_namespaces_with_rollouts();
        // Both watches restart; b finishes first, while a's relist is still open.
        on_deployment(&mut state, &ns("a"), watcher::Event::Init);
        on_deployment(&mut state, &ns("b"), watcher::Event::Init);
        on_deployment(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(deployment("a", "d1", true)),
        );
        on_deployment(
            &mut state,
            &ns("b"),
            watcher::Event::InitApply(deployment("b", "d2", true)),
        );
        on_deployment(&mut state, &ns("b"), watcher::Event::InitDone);
        assert!(has_deployment(&state, "a", "d1") && has_deployment(&state, "a", "d2"));
        assert!(!has_deployment(&state, "b", "d1") && has_deployment(&state, "b", "d2"));
        on_deployment(
            &mut state,
            &ns("a"),
            watcher::Event::InitApply(deployment("a", "d2", true)),
        );
        on_deployment(&mut state, &ns("a"), watcher::Event::InitDone);
        assert!(has_deployment(&state, "a", "d1") && has_deployment(&state, "a", "d2"));
        assert_eq!(state.deployments.len(), 3);
    }

    #[test]
    fn the_cluster_wide_deployment_watch_relists_every_namespace() {
        let mut state = whole_cluster_with_rollouts();
        on_deployment(&mut state, &CLUSTER, watcher::Event::Init);
        on_deployment(
            &mut state,
            &CLUSTER,
            watcher::Event::InitApply(deployment("b", "d2", true)),
        );
        on_deployment(&mut state, &CLUSTER, watcher::Event::InitDone);
        assert!(has_deployment(&state, "b", "d2"));
        assert_eq!(
            state.deployments.len(),
            1,
            "every unseen Deployment is dropped"
        );
        assert_eq!((state.pods.len(), state.events.len()), (4, 4));
    }

    #[test]
    fn a_forbidden_deployment_watch_drops_only_the_deployments_of_its_namespace() {
        for error in forbidden_errors() {
            let mut state = two_namespaces_with_rollouts();
            state.on_watch_error(Kind::Deployments, &ns("a"), &error);
            assert!(
                !has_deployment(&state, "a", "d1") && !has_deployment(&state, "a", "d2"),
                "{error}"
            );
            assert!(
                has_deployment(&state, "b", "d1") && has_deployment(&state, "b", "d2"),
                "{error}"
            );
            assert_eq!(
                (state.pods.len(), state.events.len()),
                (4, 4),
                "{error}: the Pods and the Events stay"
            );
            // A refused Pod or event watch leaves the Deployments.
            state.on_watch_error(Kind::Pods, &ns("b"), &error);
            state.on_watch_error(Kind::Events, &ns("b"), &error);
            assert_eq!(state.deployments.len(), 2, "{error}");
        }
        for error in forbidden_errors() {
            let mut state = whole_cluster_with_rollouts();
            state.on_watch_error(Kind::Deployments, &CLUSTER, &error);
            assert!(state.deployments.is_empty(), "{error}");
            assert_eq!((state.pods.len(), state.events.len()), (4, 4), "{error}");
        }
    }

    #[test]
    fn a_forbidden_deployment_watch_drops_its_open_relist_only() {
        for error in forbidden_errors() {
            let mut state = two_namespaces_with_rollouts();
            on_deployment(&mut state, &ns("a"), watcher::Event::Init);
            on_deployment(&mut state, &ns("b"), watcher::Event::Init);
            on_deployment(
                &mut state,
                &ns("b"),
                watcher::Event::InitApply(deployment("b", "d1", true)),
            );
            state.on_watch_error(Kind::Deployments, &ns("a"), &error);
            assert!(!state.deployment_relist.contains_key(&ns("a")), "{error}");
            assert!(state.deployment_relist.contains_key(&ns("b")), "{error}");
            on_deployment(&mut state, &ns("b"), watcher::Event::InitDone);
            assert!(
                !has_deployment(&state, "a", "d1") && !has_deployment(&state, "a", "d2"),
                "{error}"
            );
            assert!(
                has_deployment(&state, "b", "d1") && !has_deployment(&state, "b", "d2"),
                "{error}"
            );
        }
    }

    #[test]
    fn other_deployment_watch_errors_keep_the_records_and_the_open_relist() {
        let errors = vec![
            watcher::Error::InitialListFailed(api_error(500)),
            watcher::Error::WatchStartFailed(api_error(401)),
            watcher::Error::WatchStartFailed(api_error(404)),
            watcher::Error::WatchError(status(410, "Expired")),
            watcher::Error::WatchError(status(500, "InternalError")),
            watcher::Error::WatchError(status(0, "Forbidden")),
            watcher::Error::WatchFailed(kube::Error::LinesCodecMaxLineLengthExceeded),
            watcher::Error::NoResourceVersion,
        ];
        for error in errors {
            for watch in [ns("a"), CLUSTER] {
                let mut state = two_namespaces_with_rollouts();
                state.on_watch_error(Kind::Deployments, &watch, &error);
                assert_eq!(state.deployments.len(), 4, "{error} on {watch:?}");
            }
            let mut state = two_namespaces_with_rollouts();
            on_deployment(&mut state, &ns("a"), watcher::Event::Init);
            on_deployment(
                &mut state,
                &ns("a"),
                watcher::Event::InitApply(deployment("a", "d1", true)),
            );
            state.on_watch_error(Kind::Deployments, &ns("a"), &error);
            assert_eq!(state.deployments.len(), 4, "{error}: the records stay");
            on_deployment(&mut state, &ns("a"), watcher::Event::InitDone);
            assert!(
                has_deployment(&state, "a", "d1") && !has_deployment(&state, "a", "d2"),
                "{error}: the relist went on"
            );
        }
    }

    #[test]
    fn only_a_matching_deployment_is_kept() {
        let mut state = two_namespaces_with_rollouts();
        // Progress, a completed rollout or a new one: the next update no longer matches.
        on_deployment(
            &mut state,
            &ns("a"),
            watcher::Event::Apply(deployment("a", "d1", false)),
        );
        assert!(!has_deployment(&state, "a", "d1"));
        on_deployment(
            &mut state,
            &ns("b"),
            watcher::Event::InitApply(deployment("b", "d1", false)),
        );
        assert!(!has_deployment(&state, "b", "d1"));
        // A delete removes it whatever it says.
        on_deployment(
            &mut state,
            &ns("a"),
            watcher::Event::Delete(deployment("a", "d2", true)),
        );
        assert!(!has_deployment(&state, "a", "d2"));
        assert_eq!(state.deployments.len(), 1);

        // A Deployment that does not match is never kept.
        let mut state = State::default();
        for event in [
            watcher::Event::Apply(deployment("a", "d1", false)),
            watcher::Event::InitApply(deployment("a", "d2", false)),
        ] {
            on_deployment(&mut state, &ns("a"), event);
        }
        // Nor is a stalled one paused, or one whose status is older than its spec.
        let mut paused = deployment("a", "d3", true);
        paused.spec.as_mut().expect("a spec").paused = Some(true);
        let mut edited = deployment("a", "d4", true);
        edited.metadata.generation = Some(3);
        for event in [watcher::Event::Apply(paused), watcher::Event::Apply(edited)] {
            on_deployment(&mut state, &ns("a"), event);
        }
        assert!(state.deployments.is_empty());

        // A Deployment without a UID is ignored, and removes nothing.
        let mut state = two_namespaces_with_rollouts();
        let mut anonymous = deployment("a", "d1", false);
        anonymous.metadata.uid = None;
        on_deployment(
            &mut state,
            &ns("a"),
            watcher::Event::Apply(anonymous.clone()),
        );
        on_deployment(&mut state, &ns("a"), watcher::Event::Delete(anonymous));
        let mut anonymous = deployment("a", "d5", true);
        anonymous.metadata.uid = None;
        on_deployment(&mut state, &ns("a"), watcher::Event::Apply(anonymous));
        assert_eq!(state.deployments.len(), 4);
    }

    #[test]
    fn readiness_counts_the_deployment_watches() {
        // Whole cluster: 3 watches.
        let mut readiness = Readiness::new(&[], true);
        assert!(!readiness.listed(Kind::Pods, &CLUSTER));
        assert!(!readiness.listed(Kind::Events, &CLUSTER));
        assert!(
            !readiness.listed(Kind::Events, &CLUSTER),
            "a repeated InitDone of one watch counts once"
        );
        assert!(readiness.listed(Kind::Deployments, &CLUSTER));

        // 3 per namespace.
        let namespaces = vec!["a".to_owned(), "b".to_owned()];
        let mut readiness = Readiness::new(&namespaces, true);
        for (kind, watch) in [
            (Kind::Pods, ns("a")),
            (Kind::Events, ns("a")),
            (Kind::Deployments, ns("a")),
            (Kind::Deployments, ns("a")),
            (Kind::Pods, ns("b")),
            (Kind::Events, ns("b")),
        ] {
            assert!(!readiness.listed(kind, &watch), "{kind:?} {watch:?}");
        }
        assert!(readiness.listed(Kind::Deployments, &ns("b")));

        // With `--no-rollouts`: 2, or 2 per namespace.
        let mut readiness = Readiness::new(&[], false);
        assert!(!readiness.listed(Kind::Pods, &CLUSTER));
        assert!(readiness.listed(Kind::Events, &CLUSTER));
        let mut readiness = Readiness::new(&namespaces, false);
        assert!(!readiness.listed(Kind::Pods, &ns("a")));
        assert!(!readiness.listed(Kind::Events, &ns("a")));
        assert!(!readiness.listed(Kind::Pods, &ns("b")));
        assert!(readiness.listed(Kind::Events, &ns("b")));
    }

    #[test]
    fn an_ended_deployment_watch_is_a_failure_that_names_it() {
        let failure = |watch: &WatchKey| match ended(Kind::Deployments, watch) {
            RunError::Failure(message) => message,
            RunError::Output(error) => panic!("not an output error: {error}"),
        };
        assert_eq!(failure(&CLUSTER), "the deployment watch ended");
        assert_eq!(
            failure(&ns("shop")),
            "the deployment watch of namespace shop ended"
        );
    }

    /// A Pod of the Deployment `checkout` in `shop`, through its ReplicaSet (which `Parents`
    /// resolves), running and healthy.
    fn checkout_pod(name: &str) -> Pod {
        let mut pod = pod("shop", name);
        pod.metadata.owner_references = Some(vec![
            serde_json::from_value(json!({
                "apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "checkout-7fffb9bf5c",
                "uid": "uid-checkout-7fffb9bf5c", "controller": true
            }))
            .expect("an owner reference"),
        ]);
        pod
    }

    #[test]
    fn a_stalled_rollout_is_an_incident_whose_job_carries_the_record_and_no_pod() {
        let mut state = State::default();
        for name in ["checkout-7fffb9bf5c-d98v6", "checkout-7fffb9bf5c-7frqt"] {
            block_on(state.on_pod(
                &CLUSTER,
                watcher::Event::Apply(checkout_pod(name)),
                &Parents,
            ))
            .expect("the log is written");
        }
        // Another workload's Pod in the same namespace is not counted.
        on_pod(
            &mut state,
            &CLUSTER,
            watcher::Event::Apply(pod("shop", "web-1")),
        );
        let mut stalled = deployment("shop", "checkout", true);
        stalled.metadata.uid = Some("uid-checkout".into());
        on_deployment(&mut state, &CLUSTER, watcher::Event::Apply(stalled.clone()));
        let record = crate::record::DeploymentRecord::from_deployment(&stalled)
            .expect("a complete Deployment");

        let mut tracker = Tracker::new(SignedDuration::from_secs(60));
        let now = Timestamp::now();
        assert!(state.tick(&mut tracker, now).is_empty());
        let later = now
            .checked_add(SignedDuration::from_secs(60))
            .expect("a valid time");
        let incidents = state.tick(&mut tracker, later);
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        let incident = incidents.into_iter().next().expect("an incident");
        assert_eq!(
            incident.key.detection,
            crate::detect::Detection::RolloutStalled
        );
        assert_eq!(
            (
                incident.owner.api_version.as_str(),
                incident.owner.kind.as_str(),
                incident.owner.name.as_str(),
                incident.owner.uid.as_str()
            ),
            ("apps/v1", "Deployment", "checkout", "uid-checkout")
        );
        assert!(incident.pods.is_empty());

        let work = state.job(incident.clone());
        assert_eq!(work.gathered.rollout, Some(record));
        assert!(work.gathered.pods.is_empty());
        assert!(work.gathered.events.is_empty());
        assert_eq!(work.gathered.pods_total, 2);
        assert!(work.log.is_none());

        // Another incident of the same Deployment carries no rollout.
        let mut crash = incident.clone();
        crash.key.detection = crate::detect::Detection::CrashLoop;
        assert_eq!(state.job(crash).gathered.rollout, None);

        // The record is gone (the rollout progressed): the job says so.
        on_deployment(&mut state, &CLUSTER, watcher::Event::Delete(stalled));
        assert_eq!(state.job(incident).gathered.rollout, None);
    }

    // Tests of the standby and leading phases (docs/adr/0019-high-availability.md), without a
    // cluster: `Serving::serve` over a fake Lease that behaves as the API server does (a merge
    // patch under a `resourceVersion` precondition), scripted watches and scripted explanations,
    // with short timings and real time on a current-thread runtime. The stop is a script, not a
    // signal. Values are invented, and no failure message prints a value read from a Lease.

    use std::cell::Cell;
    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use serde_json::Value;

    use crate::leader::LeaseError;
    use crate::prompt::{Explanation, OutputMode};

    const ME: &str = "kausa-7d9c8b6f4d-2xkqp";
    const OTHER: &str = "kausa-7d9c8b6f4d-9wzzt";

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The production ratios, much faster, with room for a busy test machine.
    const T: Timings = Timings {
        lease_duration: Duration::from_millis(900),
        renew_deadline: Duration::from_millis(600),
        retry_period: Duration::from_millis(100),
        call_timeout: Duration::from_millis(100),
        release_timeout: Duration::from_millis(100),
    };

    /// The tick of these tests.
    const TEST_TICK: Duration = Duration::from_millis(50);

    const LEADER: &str = "kausa_leader";
    const TO_LEADER: &str = "kausa_leadership_changes_total{to=\"leader\"}";
    const TO_STANDBY: &str = "kausa_leadership_changes_total{to=\"standby\"}";
    const DROPPED: &str = "kausa_notifications_total{outcome=\"dropped\"}";

    /// The value of one series in `/metrics`, 0 when it was never incremented.
    fn counter(metrics: &Metrics, series: &str) -> u64 {
        metrics
            .render()
            .lines()
            .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
            .map_or(0, |value| value.parse().expect("a count"))
    }

    fn readyz(metrics: &Metrics) -> String {
        metrics::respond("GET /readyz HTTP/1.1", metrics).2
    }

    /// A Lease that answers as the API server does: `get` returns it, and a merge patch applies
    /// only with the current `resourceVersion` (otherwise `409`), writes the holder and moves the
    /// version. It records every call.
    struct FakeLease {
        version: Cell<u64>,
        holder: RefCell<Option<String>>,
        /// Another replica renews it: every read finds a new version.
        renewing: Cell<bool>,
        gets: Cell<usize>,
        patches: RefCell<Vec<Value>>,
    }

    impl FakeLease {
        fn new(holder: Option<&str>) -> Self {
            Self {
                version: Cell::new(1),
                holder: RefCell::new(holder.map(str::to_owned)),
                renewing: Cell::new(false),
                gets: Cell::new(0),
                patches: RefCell::new(Vec::new()),
            }
        }

        fn lease(&self) -> Lease {
            Lease {
                metadata: ObjectMeta {
                    name: Some("kausa-leader".into()),
                    namespace: Some("kausa".into()),
                    resource_version: Some(self.version.get().to_string()),
                    ..ObjectMeta::default()
                },
                spec: Some(LeaseSpec {
                    holder_identity: self.holder.borrow().clone(),
                    ..LeaseSpec::default()
                }),
            }
        }

        /// Another replica writes the Lease, naming `holder`.
        fn write(&self, holder: Option<&str>) {
            self.version.set(self.version.get() + 1);
            self.holder.replace(holder.map(str::to_owned));
        }

        fn held_by_me(&self) -> bool {
            self.holder.borrow().as_deref() == Some(ME)
        }

        fn is_free(&self) -> bool {
            self.holder.borrow().is_none()
        }

        fn calls(&self) -> usize {
            self.gets.get() + self.patches.borrow().len()
        }

        fn count(&self, kind: fn(&Value) -> bool) -> usize {
            self.patches.borrow().iter().filter(|b| kind(b)).count()
        }
    }

    fn is_acquire(body: &Value) -> bool {
        body["spec"].get("acquireTime").is_some()
    }

    fn is_renewal(body: &Value) -> bool {
        body["spec"]["holderIdentity"] == json!(ME) && !is_acquire(body)
    }

    fn is_release(body: &Value) -> bool {
        body["spec"]["holderIdentity"] == json!("")
    }

    impl LeaseApi for FakeLease {
        async fn get(&self) -> Result<Lease, LeaseError> {
            self.gets.set(self.gets.get() + 1);
            if self.renewing.get() {
                self.version.set(self.version.get() + 1);
            }
            Ok(self.lease())
        }

        async fn patch(&self, body: &Value) -> Result<Lease, LeaseError> {
            self.patches.borrow_mut().push(body.clone());
            if body["metadata"]["resourceVersion"] != json!(self.version.get().to_string()) {
                return Err(LeaseError::Conflict);
            }
            if let Some(holder) = body["spec"]["holderIdentity"].as_str() {
                self.holder
                    .replace((!holder.is_empty()).then(|| holder.to_owned()));
            }
            self.version.set(self.version.get() + 1);
            Ok(self.lease())
        }
    }

    /// What a scripted explanation does.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Explains {
        /// Ends at once.
        Done,
        /// Queues two notifications, then never ends.
        Hang,
        /// Takes a call from the budget and caches its incident, after noting whether a call was
        /// left and whether the incident was cached already.
        Remember,
    }

    /// Counts the explanations dropped while in flight.
    struct Dropped(Rc<Cell<usize>>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    /// Counts its calls; every lookup fails (the Pods of these tests have no owner).
    struct CountingLookup(Rc<Cell<usize>>);

    impl OwnerLookup for CountingLookup {
        async fn controller_of(
            &self,
            _hop: Hop,
            _namespace: &str,
            _name: &str,
        ) -> Result<Option<OwnerRef>, String> {
            self.0.set(self.0.get() + 1);
            Err("no lookup in these tests".into())
        }
    }

    /// Scripted watches and explanations. Each term's Pod watch lists `pods`, then, once `gate`
    /// is open, applies `later`, then waits or, with `ends`, ends. The event and Deployment watches
    /// list nothing.
    struct FakeCluster {
        pods: Vec<Pod>,
        later: Vec<Pod>,
        gate: Arc<AtomicBool>,
        ends: bool,
        explains: Explains,
        /// The terms started (calls of `watch`), and what the last asked about rollouts.
        watches: Cell<usize>,
        rollouts: Cell<Option<bool>>,
        /// The owner lookups made (one per term), and their calls.
        lookups: Cell<usize>,
        lookup_calls: Rc<Cell<usize>>,
        /// Each explanation started: its term and its fingerprint.
        jobs: RefCell<Vec<(usize, String)>>,
        dropped: Rc<Cell<usize>>,
        /// What each `Remember` explanation found: a call left, the incident cached.
        found: RefCell<Vec<(bool, bool)>>,
        /// The watch items the controller took.
        yielded: Arc<AtomicUsize>,
    }

    impl FakeCluster {
        fn new(pods: Vec<Pod>, explains: Explains) -> Self {
            Self {
                pods,
                later: Vec::new(),
                gate: Arc::new(AtomicBool::new(true)),
                ends: false,
                explains,
                watches: Cell::new(0),
                rollouts: Cell::new(None),
                lookups: Cell::new(0),
                lookup_calls: Rc::new(Cell::new(0)),
                jobs: RefCell::new(Vec::new()),
                dropped: Rc::new(Cell::new(0)),
                found: RefCell::new(Vec::new()),
                yielded: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn jobs(&self) -> usize {
            self.jobs.borrow().len()
        }
    }

    /// A watch on the whole cluster that lists `objects`, applies `later` once `gate` is open,
    /// and then waits or ends. Each item counts in `yielded`.
    fn scripted<K: Send + 'static>(
        objects: Vec<K>,
        later: Vec<K>,
        gate: &Arc<AtomicBool>,
        ends: bool,
        yielded: &Arc<AtomicUsize>,
    ) -> Watches<K> {
        let gate = Arc::clone(gate);
        let yielded = Arc::clone(yielded);
        let listed: Vec<watcher::Event<K>> = std::iter::once(watcher::Event::Init)
            .chain(objects.into_iter().map(watcher::Event::InitApply))
            .chain(std::iter::once(watcher::Event::InitDone))
            .collect();
        let later = stream::once(async move {
            while !gate.load(Ordering::SeqCst) {
                tokio::time::sleep(ms(5)).await;
            }
            stream::iter(later.into_iter().map(watcher::Event::Apply))
        })
        .flatten();
        let tail: BoxStream<'static, Tagged<K>> = if ends {
            stream::once(async { (None, None) }).boxed()
        } else {
            stream::pending().boxed()
        };
        let items = stream::iter(listed).chain(later).map(move |event| {
            yielded.fetch_add(1, Ordering::SeqCst);
            (None, Some(Ok(event)))
        });
        stream::select_all([items.chain(tail).boxed()])
    }

    fn explanation() -> Explanation {
        Explanation {
            summary: "an invented summary".into(),
            cause: "an invented cause".into(),
            steps: vec!["an invented step".into()],
            confidence: "low".into(),
        }
    }

    impl Cluster for FakeCluster {
        type Lookup = CountingLookup;

        fn watch(&self, namespaces: &[String], rollouts: bool) -> Watched {
            assert!(namespaces.is_empty(), "these tests watch the whole cluster");
            self.watches.set(self.watches.get() + 1);
            self.rollouts.set(Some(rollouts));
            let none = Arc::new(AtomicBool::new(true));
            Watched {
                pods: scripted(
                    self.pods.clone(),
                    self.later.clone(),
                    &self.gate,
                    self.ends,
                    &self.yielded,
                ),
                events: scripted(Vec::<Event>::new(), Vec::new(), &none, false, &self.yielded),
                deployments: rollouts.then(|| {
                    scripted(
                        Vec::<Deployment>::new(),
                        Vec::new(),
                        &none,
                        false,
                        &self.yielded,
                    )
                }),
            }
        }

        fn lookup(&self) -> CountingLookup {
            self.lookups.set(self.lookups.get() + 1);
            CountingLookup(Rc::clone(&self.lookup_calls))
        }

        fn explain<'a>(
            &'a self,
            job: Work,
            _settings: &'a Settings,
            shared: &'a Shared<'_>,
        ) -> Explaining<'a> {
            let incident = job.gathered.incident;
            self.jobs
                .borrow_mut()
                .push((self.watches.get(), incident.fingerprint.clone()));
            Box::pin(async move {
                match self.explains {
                    Explains::Done => {}
                    Explains::Hang => {
                        let _dropped = Dropped(Rc::clone(&self.dropped));
                        for _ in 0..2 {
                            shared
                                .notifications
                                .borrow_mut()
                                .push_back(notify::queued(&incident, explanation()));
                        }
                        std::future::pending::<()>().await;
                    }
                    Explains::Remember => {
                        let now = Instant::now();
                        let left = shared.budget.borrow_mut().take(now).is_ok();
                        let cached = shared.dedup.borrow().peek(&incident.fingerprint).is_some();
                        self.found.borrow_mut().push((left, cached));
                        shared.dedup.borrow_mut().put(
                            &incident.fingerprint,
                            explanation(),
                            None,
                            now,
                        );
                    }
                }
                Ok(None)
            })
        }
    }

    // Invented values only: no real cluster output (AGENTS.md, Security rules).
    fn crashing(name: &str) -> Pod {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": "shop", "uid": pod_uid("shop", name)},
            "spec": {"containers": [{"name": "app", "image": "registry.example.com/app:1"}]},
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "app", "image": "registry.example.com/app:1", "imageID": "",
                    "ready": false, "restartCount": 6,
                    "state": {"waiting": {"reason": "CrashLoopBackOff",
                                          "message": "back-off 2m40s restarting failed container"}},
                    "lastState": {"terminated": {"reason": "Error", "exitCode": 1}}
                }]
            }
        }))
        .expect("the fixture is a valid Pod")
    }

    fn options(lease: bool, calls_per_hour: usize) -> Options {
        Options {
            settle: SignedDuration::ZERO,
            explain: Some(Settings {
                model: None,
                model_name: "m".into(),
                logs: false,
                mode: OutputMode::JsonSchema,
                max_tokens: 800,
                max_input_tokens: 6000,
                events: false,
                history_days: None,
                calls_per_hour,
                calls_per_day: 200,
                dedup_ttl: Duration::from_secs(3600),
                instance: ME.into(),
                notify: None,
            }),
            metrics_address: None,
            namespaces: Vec::new(),
            rollouts: true,
            leader_lease: lease.then(|| LeaderLease {
                namespace: "kausa".into(),
                name: "kausa-leader".into(),
                identity: ME.into(),
            }),
        }
    }

    fn shared_of<'a>(options: &Options, metrics: &'a Metrics, term: &'a Term) -> Shared<'a> {
        let settings = options.explain.as_ref().expect("settings");
        Shared {
            budget: RefCell::new(Budget::new(settings.calls_per_hour, settings.calls_per_day)),
            dedup: RefCell::new(Dedup::new(settings.dedup_ttl, DEDUP_CAPACITY)),
            writer: None,
            history: None,
            metrics,
            notifications: RefCell::new(VecDeque::new()),
            term,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
    }

    /// Serves until `script` ends, as `run` does after it has connected.
    async fn serve(
        options: &Options,
        lease: &FakeLease,
        cluster: &FakeCluster,
        shared: &Shared<'_>,
        listener: Option<&TcpListener>,
        script: impl Future<Output = ()>,
    ) -> Result<(), RunError> {
        Serving {
            options,
            lease: Some(lease),
            cluster,
            shared: Some(shared),
            metrics: shared.metrics,
            term: shared.term,
            listener,
            timings: T,
            tick: TEST_TICK,
        }
        .serve(script)
        .await
    }

    /// Waits until `done`, at most `limit`; says whether it happened.
    async fn until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let start = Instant::now();
        while !done() {
            if start.elapsed() > limit {
                return false;
            }
            tokio::time::sleep(ms(5)).await;
        }
        true
    }

    fn served(result: Result<(), RunError>) {
        match result {
            Ok(()) => {}
            Err(RunError::Failure(error)) => panic!("stopped on an error: {error}"),
            Err(RunError::Output(error)) => panic!("the log failed: {error}"),
        }
    }

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn a_standby_starts_no_watch_and_no_owner_lookup() {
        let options = options(true, 10);
        let (metrics, term) = (Metrics::default(), Term::closed());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(Some(OTHER));
        lease.renewing.set(true);
        let cluster = FakeCluster::new(vec![crashing("api-1")], Explains::Done);
        let result = runtime().block_on(serve(&options, &lease, &cluster, &shared, None, async {
            assert!(until(WAIT, || lease.gets.get() >= 4).await, "four reads");
        }));
        served(result);
        assert_eq!(cluster.watches.get(), 0, "no watch");
        assert_eq!(cluster.lookups.get(), 0, "no owner lookup");
        assert_eq!(cluster.jobs(), 0, "no explanation");
        assert!(
            lease.patches.borrow().is_empty(),
            "no acquire and no release"
        );
        assert!(!term.holds());
        assert!(metrics.is_ready() && !metrics.is_leading());
        assert_eq!(readyz(&metrics), "standby\n");
        assert_eq!(counter(&metrics, LEADER), 0);
        assert_eq!(counter(&metrics, TO_LEADER), 0);
    }

    #[test]
    fn acquiring_the_lease_starts_the_watches_of_a_term() {
        let options = options(true, 10);
        let (metrics, term) = (Metrics::default(), Term::closed());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(None);
        let cluster = FakeCluster::new(vec![pod("shop", "web-1")], Explains::Done);
        let result = runtime().block_on(serve(&options, &lease, &cluster, &shared, None, async {
            assert!(
                until(WAIT, || metrics.is_ready() && lease.count(is_renewal) >= 2).await,
                "listed and renewed"
            );
            assert_eq!(cluster.watches.get(), 1, "one term's watches");
            assert_eq!(
                cluster.rollouts.get(),
                Some(true),
                "with the Deployment watch"
            );
            assert_eq!(cluster.lookups.get(), 1, "a fresh owner lookup");
            assert!(lease.held_by_me() && term.holds());
            assert!(metrics.is_leading());
            assert_eq!(readyz(&metrics), "ready\n");
            assert_eq!(counter(&metrics, LEADER), 1);
            assert_eq!(counter(&metrics, TO_LEADER), 1);
        }));
        served(result);
        assert_eq!(lease.count(is_acquire), 1, "acquired at the first read");
        assert_eq!(counter(&metrics, TO_STANDBY), 0);
    }

    #[test]
    fn a_stop_while_leading_releases_the_lease_exactly_once() {
        let options = options(true, 10);
        let (metrics, term) = (Metrics::default(), Term::closed());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(None);
        let cluster = FakeCluster::new(vec![pod("shop", "web-1")], Explains::Done);
        let result = runtime().block_on(serve(&options, &lease, &cluster, &shared, None, async {
            assert!(
                until(WAIT, || lease.count(is_renewal) >= 3).await,
                "renewed three times"
            );
        }));
        served(result);
        assert_eq!(lease.count(is_release), 1, "one release");
        let last = lease.patches.borrow().last().cloned().expect("a patch");
        assert!(is_release(&last), "nothing is sent after the release");
        // The fake applies a patch only with the current version: the release used the last one.
        assert!(lease.is_free(), "the release was applied");
        assert!(!term.holds(), "nothing writes after the release");
    }

    #[test]
    fn a_lost_lease_drops_the_explanation_in_flight_and_the_queued_notifications() {
        let options = options(true, 10);
        let (metrics, term) = (Metrics::default(), Term::closed());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(None);
        let cluster = FakeCluster::new(vec![crashing("api-1")], Explains::Hang);
        let dropped_at_step_down = Cell::new(None);
        let result = runtime().block_on(serve(&options, &lease, &cluster, &shared, None, async {
            assert!(
                until(WAIT, || shared.notifications.borrow().len() == 2).await,
                "an explanation in flight, two notifications queued"
            );
            // Another replica takes the Lease and keeps renewing it.
            lease.renewing.set(true);
            lease.write(Some(OTHER));
            assert!(
                until(WAIT, || counter(&metrics, TO_STANDBY) == 1).await,
                "stepped down"
            );
            dropped_at_step_down.set(Some(cluster.dropped.get()));
            let reads = lease.gets.get();
            assert!(
                until(WAIT, || lease.gets.get() >= reads + 2).await,
                "reads as a standby"
            );
        }));
        served(result);
        assert_eq!(
            dropped_at_step_down.get(),
            Some(1),
            "dropped at the step-down"
        );
        assert_eq!(cluster.jobs(), 1, "never started again");
        assert!(shared.notifications.borrow().is_empty());
        assert_eq!(counter(&metrics, DROPPED), 2);
        assert_eq!(cluster.watches.get(), 1, "a standby starts no watch");
        assert!(!term.holds() && !metrics.is_leading());
        assert_eq!(counter(&metrics, LEADER), 0);
        assert_eq!(readyz(&metrics), "standby\n");
        assert_eq!(lease.count(is_release), 0, "a lost Lease is not released");
    }

    #[test]
    fn a_second_term_keeps_the_budget_and_the_dedup_entry() {
        // One call an hour: the second term finds it spent, and the incident cached.
        let options = options(true, 1);
        let (metrics, term) = (Metrics::default(), Term::closed());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(None);
        let cluster = FakeCluster::new(vec![crashing("api-1")], Explains::Remember);
        let result = runtime().block_on(serve(&options, &lease, &cluster, &shared, None, async {
            assert!(
                until(WAIT, || cluster.found.borrow().len() == 1).await,
                "explained in the first term"
            );
            // Another replica takes the Lease and dies: it is taken back after lease_duration.
            lease.write(Some(OTHER));
            assert!(
                until(WAIT, || cluster.found.borrow().len() == 2).await,
                "explained again in the second term"
            );
        }));
        served(result);
        assert_eq!(
            *cluster.found.borrow(),
            vec![(true, false), (false, true)],
            "the second term saw the spent call and the cached incident"
        );
        let jobs = cluster.jobs.borrow();
        assert_eq!((jobs[0].0, jobs[1].0), (1, 2), "one explanation per term");
        assert_eq!(jobs[0].1, jobs[1].1, "of the same incident");
        assert_eq!(cluster.lookups.get(), 2, "a fresh owner lookup per term");
        assert_eq!(counter(&metrics, TO_LEADER), 2);
        assert_eq!(counter(&metrics, TO_STANDBY), 1);
    }

    #[test]
    fn an_ended_watch_while_leading_releases_the_lease_and_fails() {
        let options = options(true, 10);
        let (metrics, term) = (Metrics::default(), Term::closed());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(None);
        let mut cluster = FakeCluster::new(vec![pod("shop", "web-1")], Explains::Done);
        cluster.ends = true;
        let result = runtime().block_on(serve(&options, &lease, &cluster, &shared, None, async {
            until(WAIT, || false).await;
        }));
        match result {
            Err(RunError::Failure(error)) => assert_eq!(error, "the pod watch ended"),
            Err(RunError::Output(error)) => panic!("the log failed: {error}"),
            Ok(()) => panic!("the ended watch did not stop kausa"),
        }
        assert_eq!(lease.count(is_acquire), 1);
        assert_eq!(lease.count(is_release), 1, "released once");
        assert!(lease.is_free(), "the release was applied");
        assert!(!term.holds());
    }

    #[test]
    fn without_a_leader_lease_no_lease_call_is_made() {
        let options = options(false, 10);
        let (metrics, term) = (Metrics::default(), Term::unbounded());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(None);
        let cluster = FakeCluster::new(vec![crashing("api-1")], Explains::Done);
        let result = runtime().block_on(serve(&options, &lease, &cluster, &shared, None, async {
            assert!(
                until(WAIT, || metrics.is_ready() && cluster.jobs() == 1).await,
                "listed and explained"
            );
        }));
        served(result);
        assert_eq!(lease.calls(), 0, "no Lease call");
        assert!(term.holds(), "the term never ends");
        assert!(metrics.is_leading());
        assert_eq!(readyz(&metrics), "ready\n");
        assert_eq!(counter(&metrics, LEADER), 1);
        assert_eq!(counter(&metrics, TO_LEADER), 0);
        assert_eq!(cluster.watches.get(), 1);
    }

    /// Asks for `/metrics` over and over until `stop`, counting the answers.
    fn flood(address: SocketAddr, stop: &AtomicBool, answered: &AtomicUsize) {
        while !stop.load(Ordering::SeqCst) {
            let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, ms(200)) else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(ms(200)));
            let _ = stream.set_write_timeout(Some(ms(200)));
            if stream.write_all(b"GET /metrics HTTP/1.1\r\n\r\n").is_err() {
                continue;
            }
            let mut answer = Vec::new();
            if stream.read_to_end(&mut answer).is_ok() && answer.starts_with(b"HTTP/1.1 200") {
                answered.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn a_metrics_flood_stops_neither_the_controller_nor_the_keeper() {
        let options = options(true, 10);
        let (metrics, term) = (Metrics::default(), Term::closed());
        let shared = shared_of(&options, &metrics, &term);
        let lease = FakeLease::new(None);
        let listed: Vec<Pod> = (0..200).map(|n| pod("shop", &format!("web-{n}"))).collect();
        let mut cluster = FakeCluster::new(listed, Explains::Done);
        // After the flood has begun: 100 updates and one crashing Pod.
        cluster.later = (0..100).map(|n| pod("shop", &format!("web-{n}"))).collect();
        cluster.later.push(crashing("api-1"));
        cluster.gate = Arc::new(AtomicBool::new(false));
        let items = 1 + 200 + 1 + 101 + 2 + 2;
        let stop = Arc::new(AtomicBool::new(false));
        let answered = Arc::new(AtomicUsize::new(0));
        let runtime = runtime();
        let (result, explained_after) = runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
            let address = listener.local_addr().expect("an address");
            let clients: Vec<_> = (0..8)
                .map(|_| {
                    let (stop, answered) = (Arc::clone(&stop), Arc::clone(&answered));
                    std::thread::spawn(move || flood(address, &stop, &answered))
                })
                .collect();
            let explained_after = Cell::new(None);
            let result = serve(
                &options,
                &lease,
                &cluster,
                &shared,
                Some(&listener),
                async {
                    assert!(
                        until(WAIT, || answered.load(Ordering::SeqCst) >= 50).await,
                        "the flood is on"
                    );
                    assert!(metrics.is_leading(), "leading under the flood");
                    let renewals = lease.count(is_renewal);
                    let opened = Instant::now();
                    cluster.gate.store(true, Ordering::SeqCst);
                    assert!(until(WAIT, || cluster.jobs() == 1).await, "explained");
                    explained_after.set(Some(opened.elapsed()));
                    assert!(
                        until(WAIT, || lease.count(is_renewal) >= renewals + 3).await,
                        "renewed under the flood"
                    );
                },
            )
            .await;
            stop.store(true, Ordering::SeqCst);
            drop(listener);
            for client in clients {
                client.join().expect("the client thread");
            }
            (result, explained_after.get())
        });
        served(result);
        assert_eq!(
            cluster.yielded.load(Ordering::SeqCst),
            items,
            "every watch item was handled"
        );
        let explained_after = explained_after.expect("explained");
        assert!(
            explained_after <= TEST_TICK * 10,
            "explained {explained_after:?} after the incident appeared"
        );
        assert_eq!(
            counter(&metrics, TO_STANDBY),
            0,
            "the keeper never lost the Lease"
        );
        assert!(answered.load(Ordering::SeqCst) >= 50);
    }
}
