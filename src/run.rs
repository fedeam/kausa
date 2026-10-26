//! `kausa run`: the controller loop. It watches Pods and Warning events, keeps small records,
//! correlates owners and logs each incident when it settles.
//!
//! Status as of 2026-10-26 (second implementation weekend): incidents are written to kausa's log
//! and, with `--endpoint` or `--dry-run`, explained by the model or their request logged
//! (`explain.rs`). No Event is written yet; the API calls are the two watches, the metadata reads
//! of ReplicaSets and Jobs, and `pods/log` for `D001`, `D003` and `D008` unless `--no-logs`
//! (docs/reference/permissions.md).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::{Duration, Instant};

use futures::StreamExt;
use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::api::events::v1::Event;
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use kube::config::KubeConfigOptions;
use kube::runtime::{WatchStreamExt, watcher};
use kube::{Api, Client, Config};
use serde_json::json;
use tokio::signal::unix::{SignalKind, signal};

use crate::context::Gathered;
use crate::detect::{Detection, detect_event};
use crate::explain::{self, LogSource, QUEUE_LIMIT, Settings, Work};
use crate::incident::{EventView, Incident, PodView, Tracker, collect_signals};
use crate::log;
use crate::owner::{Hop, OwnerLookup, top_owner};
use crate::record::{EventRecord, OwnerRef, PodRecord, controller_of};

/// The reporting controller of the Events kausa will write, excluded from its own watch.
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
/// How long one owner lookup may take before it counts as failed.
const OWNER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Options {
    pub settle: SignedDuration,
    /// How to explain incidents; `None` logs them only.
    pub explain: Option<Settings>,
}

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
    let mut pods = watcher(Api::<Pod>::all(client.clone()), watcher::Config::default())
        .default_backoff()
        .boxed();
    let selector = format!("type=Warning,reportingController!={REPORTING_CONTROLLER}");
    let mut events = watcher(
        Api::<Event>::all(client),
        watcher::Config::default().fields(&selector),
    )
    .default_backoff()
    .boxed();
    let mut state = State::default();
    let mut tracker = Tracker::new(options.settle);
    let mut ticker = tokio::time::interval(TICK);
    let mut queue: VecDeque<Work> = VecDeque::new();
    let mut running: Option<Pin<Box<dyn Future<Output = io::Result<()>> + '_>>> = None;

    loop {
        if running.is_none()
            && let (Some(settings), Some(job)) = (options.explain.as_ref(), queue.pop_front())
        {
            running = Some(Box::pin(explain::run(job, explainer.clone(), settings)));
        }
        tokio::select! {
            result = async { running.as_mut().expect("guarded by the condition").await },
                if running.is_some() => {
                running = None;
                result?;
            }
            item = pods.next() => match item {
                Some(Ok(event)) => state.on_pod(event, &lookup).await?,
                Some(Err(e)) => log::warn("pod watch error", json!({"error": e.to_string()}))?,
                None => return Err(RunError::Failure("the pod watch ended".into())),
            },
            item = events.next() => match item {
                Some(Ok(event)) => state.on_event(event, &lookup).await?,
                Some(Err(e)) => log::warn("event watch error", json!({"error": e.to_string()}))?,
                None => return Err(RunError::Failure("the event watch ended".into())),
            },
            _ = ticker.tick() => {
                let now = Timestamp::now();
                state.prune_events(now);
                for incident in state.tick(&mut tracker, now) {
                    log_incident(&incident, state.pods_of(&incident.owner))?;
                    if options.explain.is_some() {
                        if queue.len() >= QUEUE_LIMIT
                            && let Some(dropped) = queue.pop_front()
                        {
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
    /// The UIDs seen during a relist of each watch, to drop the ones that disappeared.
    pod_relist: Option<HashSet<String>>,
    event_relist: Option<HashSet<String>>,
}

impl State {
    async fn on_pod<L: OwnerLookup>(
        &mut self,
        event: watcher::Event<Pod>,
        lookup: &L,
    ) -> io::Result<()> {
        match event {
            watcher::Event::Init => self.pod_relist = Some(HashSet::new()),
            watcher::Event::InitDone => {
                if let Some(seen) = self.pod_relist.take() {
                    self.pods.retain(|uid, _| seen.contains(uid));
                }
            }
            watcher::Event::Apply(pod) | watcher::Event::InitApply(pod) => {
                let Some(record) = PodRecord::from_pod(&pod) else {
                    return Ok(());
                };
                if let Some(seen) = &mut self.pod_relist {
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

    async fn on_event<L: OwnerLookup>(
        &mut self,
        event: watcher::Event<Event>,
        lookup: &L,
    ) -> io::Result<()> {
        match event {
            watcher::Event::Init => self.event_relist = Some(HashSet::new()),
            watcher::Event::InitDone => {
                if let Some(seen) = self.event_relist.take() {
                    self.events.retain(|uid, _| seen.contains(uid));
                }
            }
            watcher::Event::Apply(event) | watcher::Event::InitApply(event) => {
                let Some(record) = EventRecord::from_event(&event) else {
                    return Ok(());
                };
                if let Some(seen) = &mut self.event_relist {
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
