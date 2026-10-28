//! `kausa run`: the controller loop. It watches Pods and Warning events, keeps small records,
//! correlates owners and logs each incident when it settles.
//!
//! Status as of 2026-10-28 (v0.2.0 and the namespaced mode planned for v0.3): incidents are
//! written to kausa's log and, with `--endpoint` or `--dry-run`, explained by the model or their
//! request logged (`explain.rs`); with `--endpoint` and without `--no-events`, the explanation is also an
//! Event (`events.rs`). The API calls are the watches (one of Pods and one of Warning events on
//! the whole cluster, or one of each per watched namespace), the metadata reads of ReplicaSets and
//! Jobs, `pods/log` for `D001`, `D003` and `D008` unless `--no-logs`, and the Event writes
//! (docs/reference/permissions.md). `--metrics-address` serves the counters (`metrics.rs`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use futures::stream::{self, BoxStream, FuturesUnordered, SelectAll};
use futures::{Stream, StreamExt};
use k8s_openapi::api::apps::v1::ReplicaSet;
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
use tokio::signal::unix::{SignalKind, signal};

use crate::budget::{Budget, Dedup};
use crate::context::Gathered;
use crate::detect::{Detection, detect_event};
use crate::events::Writer;
use crate::explain::{self, LogSource, QUEUE_LIMIT, Settings, Shared, Work};
use crate::incident::{EventView, Incident, PodView, Tracker, collect_signals};
use crate::log;
use crate::metrics::{self, Metrics};
use crate::owner::{Hop, OwnerLookup, top_owner};
use crate::record::{EventRecord, OwnerRef, PodRecord, controller_of};

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
}

/// Which watch an item comes from: `None` is the cluster-wide watch, `Some(n)` the watch of
/// namespace `n` (docs/adr/0013-namespaced-install-mode.md).
pub type WatchKey = Option<String>;

/// Which kind of object a watch reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Pods,
    Events,
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

/// Which watches have sent `InitDone` at least once. kausa is ready once every watch has: 2 on
/// the whole cluster, or 2 per namespace (docs/adr/0013-namespaced-install-mode.md).
struct Readiness {
    total: usize,
    listed: HashSet<(Kind, WatchKey)>,
}

impl Readiness {
    /// The watches `run` starts for `namespaces`: one Pod and one event watch per namespace, or
    /// one of each on the whole cluster when the list is empty.
    fn new(namespaces: &[String]) -> Self {
        Readiness {
            total: 2 * namespaces.len().max(1),
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
        }),
    )?;

    let listen = |kind, name: &str| {
        signal(kind).map_err(|e| RunError::Failure(format!("cannot listen for {name}: {e}")))
    };
    let mut terminate = listen(SignalKind::terminate(), "SIGTERM")?;
    let mut interrupt = listen(SignalKind::interrupt(), "SIGINT")?;
    let lookup = KubeLookup {
        client: client.clone(),
        cache: RefCell::new(HashMap::new()),
    };
    let explainer = client.clone();
    let metrics = Metrics::default();
    let listener = match &options.metrics_address {
        Some(address) => Some(
            tokio::net::TcpListener::bind(address)
                .await
                .map_err(|e| RunError::Failure(format!("cannot listen on {address}: {e}")))?,
        ),
        None => None,
    };
    let shared = options.explain.as_ref().map(|settings| Shared {
        budget: RefCell::new(Budget::new(settings.calls_per_hour, settings.calls_per_day)),
        dedup: RefCell::new(Dedup::new(settings.dedup_ttl, DEDUP_CAPACITY)),
        writer: (settings.events && settings.model.is_some())
            .then(|| Writer::new(client.clone(), settings.instance.clone())),
        metrics: &metrics,
    });
    let mut readiness = Readiness::new(&options.namespaces);
    let mut pods = watches::<Pod>(&client, &options.namespaces, &watcher::Config::default());
    let selector = format!("type=Warning,reportingController!={REPORTING_CONTROLLER}");
    let mut events = watches::<Event>(
        &client,
        &options.namespaces,
        &watcher::Config::default().fields(&selector),
    );
    let mut state = State::default();
    let mut tracker = Tracker::new(options.settle);
    let mut ticker = tokio::time::interval(TICK);
    let mut queue: VecDeque<Work> = VecDeque::new();
    let mut running: Option<Explaining<'_>> = None;
    // Jobs whose first call failed with a transient error, and when they may be tried again.
    let mut delayed: Vec<(Instant, Work)> = Vec::new();
    let mut connections = FuturesUnordered::new();

    loop {
        if running.is_none()
            && let (Some(settings), Some(shared), Some(job)) =
                (options.explain.as_ref(), shared.as_ref(), queue.pop_front())
        {
            running = Some(Box::pin(explain::run(
                job,
                explainer.clone(),
                settings,
                shared,
            )));
        }
        tokio::select! {
            result = async { running.as_mut().expect("guarded by the condition").await },
                if running.is_some() => {
                running = None;
                if let Some(job) = result? {
                    delayed.push((Instant::now() + explain::RETRY_DELAY, job));
                }
            }
            accepted = async { listener.as_ref().expect("guarded by the condition").accept().await },
                if listener.is_some() && connections.len() < MAX_CONNECTIONS => match accepted {
                Ok((stream, _)) => connections.push(metrics::answer(stream, &metrics)),
                Err(e) => {
                    log::warn("metrics accept error", json!({"error": e.to_string()}))?;
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(()) = connections.next(), if !connections.is_empty() => {}
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
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
    }
    log::info("stopped", json!({}))?;
    Ok(())
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
    /// Only Warning events that a detection matches, with the Job's top-level owner for `D008`.
    events: HashMap<String, (EventRecord, Option<OwnerRef>)>,
    /// The UIDs seen during the open relist of each watch, to drop the ones that disappeared
    /// from that watch.
    pod_relist: HashMap<WatchKey, HashSet<String>>,
    event_relist: HashMap<WatchKey, HashSet<String>>,
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
                let job_owner = if detection == Some(Detection::JobFailed) {
                    let job = OwnerRef {
                        api_version: record.regarding.api_version.clone(),
                        kind: record.regarding.kind.clone(),
                        name: record.regarding.name.clone(),
                        uid: record.regarding.uid.clone().unwrap_or_default(),
                    };
                    let namespace = record.regarding.namespace.clone();
                    let (owner, error) = top_owner(lookup, &namespace, Some(&job), &job).await;
                    if let Some(error) = error {
                        log::warn(
                            "owner lookup failed",
                            json!({"namespace": namespace, "job": job.name, "error": error}),
                        )?;
                    }
                    Some(owner)
                } else {
                    None
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
        tracker.tick(now, collect_signals(&pods, &events, now, EVENT_FRESHNESS))
    }

    /// What an explanation needs about an incident, copied out of the state: the incident's Pods
    /// (sample first), their events, and which log to read.
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
        Work {
            gathered: Gathered {
                pods_total: self.pods_of(&incident.owner),
                incident,
                pods,
                events,
                log_tail: None,
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

/// A cached owner lookup: the answer (or the error) and when it was read.
/// The explanation in progress: it ends with a job to retry, if its call failed transiently.
type Explaining<'a> = Pin<Box<dyn Future<Output = io::Result<Option<Work>>> + 'a>>;

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
        let mut readiness = Readiness::new(&namespaces);
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
        let mut readiness = Readiness::new(&[]);
        assert!(!readiness.listed(Kind::Events, &CLUSTER));
        assert!(!readiness.listed(Kind::Events, &CLUSTER));
        assert!(readiness.listed(Kind::Pods, &CLUSTER));
        assert!(readiness.listed(Kind::Pods, &CLUSTER));
    }

    #[test]
    fn readiness_with_one_namespace_needs_its_two_watches() {
        let mut readiness = Readiness::new(&["shop".to_owned()]);
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
    fn watch_error_lines_name_the_namespace_or_null() {
        let field = |watch: &WatchKey| json!({"namespace": watch})["namespace"].clone();
        assert_eq!(field(&ns("shop")), json!("shop"));
        assert_eq!(field(&CLUSTER), serde_json::Value::Null);
    }
}
