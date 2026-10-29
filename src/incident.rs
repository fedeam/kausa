//! From detections to incidents: signals, grouping by top-level owner, and the settle window.
//!
//! Everything here is a pure function of its inputs and of the time it is given, so the same
//! cluster state always produces the same incidents and fingerprints.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use k8s_openapi::jiff::{SignedDuration, Timestamp};

use crate::detect::{
    Detection, detect_event, detect_evicted, detect_pod_container, detect_unschedulable,
};
use crate::fingerprint::{fingerprint, normalize};
use crate::record::{EventRecord, OwnerRef, PodRecord};

/// How long a state signal (a container state, a Pod that cannot be scheduled or was evicted) may
/// be missing
/// before its failure is forgotten. Between two back-offs a crash-looping container runs for a
/// while, and the kubelet's back-off grows to five minutes, so the gap has to be longer than that.
pub const STATE_GRACE: SignedDuration = SignedDuration::from_secs(330);

/// What makes two failures the same incident: the namespace, the top-level owner and the
/// detection.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IncidentKey {
    pub namespace: String,
    pub owner_kind: String,
    pub owner_name: String,
    pub detection: Detection,
}

/// One observation of a failure: a state read from a Pod, or a Warning event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    pub key: IncidentKey,
    pub owner: OwnerRef,
    /// When it was observed: now, for a state; the event's last occurrence, for an event.
    pub observed_at: Timestamp,
    /// For an event, its UID and, when its own series shows it repeated, when it first happened.
    pub event: Option<(String, Option<Timestamp>)>,
    pub pod: Option<String>,
    pub container: Option<String>,
    pub image: Option<String>,
    /// The waiting or termination reason, the `PodScheduled` condition's reason (`Unschedulable`
    /// or `SchedulerError`), `Evicted`, or the event reason.
    pub reason: String,
    pub message: String,
    pub exit_code: Option<i32>,
}

/// A Pod record and the top-level owner it was correlated to.
pub struct PodView<'a> {
    pub record: &'a PodRecord,
    pub owner: &'a OwnerRef,
}

/// An event record and, for an event about an object that is not a Pod (a Job, a ReplicaSet), that
/// object's top-level owner when `run.rs` resolved it (events about Pods take their Pod's).
pub struct EventView<'a> {
    pub record: &'a EventRecord,
    pub job_owner: Option<&'a OwnerRef>,
}

/// All the signals in the current state: one per failing container, per Pod that cannot be
/// scheduled and per recently evicted Pod, and one per recent Warning event that a detection
/// matches. Events whose last occurrence, and evictions whose failure time, is older than
/// `freshness` are ignored, and so is an eviction with no failure time: evicted Pods are not
/// deleted, and an old one must not become an incident again each time kausa starts. A timestamp
/// in the future counts as now.
pub fn collect_signals(
    pods: &[PodView<'_>],
    events: &[EventView<'_>],
    now: Timestamp,
    freshness: SignedDuration,
) -> Vec<Signal> {
    let mut signals = Vec::new();
    let mut by_name: HashMap<(&str, &str), &PodView<'_>> = HashMap::new();
    for pod in pods {
        let record = pod.record;
        by_name.insert((&record.namespace, &record.name), pod);
        let state =
            |detection, container: Option<&str>, image, reason, message, exit_code| Signal {
                key: key(&record.namespace, pod.owner, detection),
                owner: pod.owner.clone(),
                observed_at: now,
                event: None,
                pod: Some(record.name.clone()),
                container: container.map(str::to_owned),
                image,
                reason,
                message,
                exit_code,
            };
        if detect_unschedulable(record) {
            signals.push(state(
                Detection::Unschedulable,
                None,
                None,
                record
                    .unschedulable_reason
                    .clone()
                    .unwrap_or_else(|| "Unschedulable".to_owned()),
                record.unschedulable.clone().unwrap_or_default(),
                None,
            ));
        }
        let recent = |at: Option<Timestamp>| {
            at.is_some_and(|at| now.duration_since(at.min(now)) <= freshness)
        };
        if detect_evicted(record) && recent(record.failed_at) {
            signals.push(state(
                Detection::PodEvicted,
                None,
                None,
                "Evicted".to_owned(),
                record.message.clone().unwrap_or_default(),
                None,
            ));
        }
        for container in &record.containers {
            let Some(detection) = detect_pod_container(record, container) else {
                continue;
            };
            let (reason, exit_code) = if detection == Detection::OutOfMemory {
                let code = container
                    .last_termination_exit_code
                    .or(container.terminated_exit_code);
                ("OOMKilled".to_owned(), code)
            } else {
                let reason = container
                    .waiting_reason
                    .clone()
                    .or_else(|| container.terminated_reason.clone())
                    .unwrap_or_default();
                let code = container
                    .terminated_exit_code
                    .or(container.last_termination_exit_code);
                (reason, code)
            };
            signals.push(state(
                detection,
                Some(&container.name),
                container.image.clone(),
                reason,
                container.waiting_message.clone().unwrap_or_default(),
                exit_code,
            ));
        }
    }
    for event in events {
        let record = event.record;
        let Some(detection) = detect_event(record) else {
            continue;
        };
        let Some(last_seen) = record.last_seen.map(|seen| seen.min(now)) else {
            continue;
        };
        if now.duration_since(last_seen) > freshness {
            continue;
        }
        let regarding = &record.regarding;
        // An event about an object that is not a Pod (`D008`, `D011` to `D013`) needs no Pod: it is
        // about the object's top-level owner, or the object itself when there is none or it is not
        // known (a claim, an autoscaler).
        let (owner, pod) = if regarding.kind != "Pod" {
            let fallback = OwnerRef {
                api_version: regarding.api_version.clone(),
                kind: regarding.kind.clone(),
                name: regarding.name.clone(),
                uid: regarding.uid.clone().unwrap_or_default(),
            };
            (event.job_owner.cloned().unwrap_or(fallback), None)
        } else {
            let Some(pod) = by_name.get(&(regarding.namespace.as_str(), regarding.name.as_str()))
            else {
                continue; // the Pod is gone, or not seen yet
            };
            (pod.owner.clone(), Some(pod.record.name.clone()))
        };
        let series_first = (record.count > 1)
            .then_some(record.first_seen)
            .flatten()
            .map(|first| first.min(last_seen));
        signals.push(Signal {
            key: key(&regarding.namespace, &owner, detection),
            owner,
            observed_at: last_seen,
            event: Some((record.uid.clone(), series_first)),
            pod,
            container: None,
            image: None,
            reason: record.reason.clone(),
            message: record.note.clone().unwrap_or_default(),
            exit_code: None,
        });
    }
    signals
}

fn key(namespace: &str, owner: &OwnerRef, detection: Detection) -> IncidentKey {
    IncidentKey {
        namespace: namespace.to_owned(),
        owner_kind: owner.kind.clone(),
        owner_name: owner.name.clone(),
        detection,
    }
}

/// An incident: a failure of one workload that persisted through the settle window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incident {
    pub key: IncidentKey,
    pub owner: OwnerRef,
    pub fingerprint: String,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
    /// The Pods whose failures make up the incident, sorted.
    pub pods: BTreeSet<String>,
    /// The first signal in a fixed order (by Pod, container, reason, message), as the example.
    pub sample: Signal,
}

#[derive(Debug)]
struct Pending {
    first_seen: Timestamp,
    last_seen: Timestamp,
    /// The last check that had a signal for this failure.
    last_signal_at: Timestamp,
    /// Every Pod seen failing while the failure was pending, so that replicas whose crash loops
    /// are out of step all count.
    pods: BTreeSet<String>,
    emitted: bool,
}

/// Tracks failures from one check to the next and says when one becomes an incident.
#[derive(Debug)]
pub struct Tracker {
    settle: SignedDuration,
    pending: BTreeMap<IncidentKey, Pending>,
    /// When kausa first saw each Warning event that is still a signal, by UID.
    event_first_seen: HashMap<String, Timestamp>,
}

impl Tracker {
    pub fn new(settle: SignedDuration) -> Self {
        Self {
            settle,
            pending: BTreeMap::new(),
            event_first_seen: HashMap::new(),
        }
    }

    /// Feeds the signals of the check at `now`. Returns the incidents that settled now, each once
    /// while the failure lasts.
    ///
    /// - A state settles when it has been present over at least the settle window. It is
    ///   forgotten when it has been missing for longer than `STATE_GRACE`, so the moments a
    ///   crash-looping container runs between back-offs do not split one failure in several.
    /// - An event settles when the same event happened again at least a settle window after it
    ///   was first seen, by kausa or in its own series. Events of different Pods never add up.
    ///   It is forgotten as soon as no recent event remains.
    /// - A failed Job settles at once.
    pub fn tick(&mut self, now: Timestamp, signals: Vec<Signal>) -> Vec<Incident> {
        let mut groups: BTreeMap<IncidentKey, Vec<Signal>> = BTreeMap::new();
        let mut live_events = HashSet::new();
        for signal in signals {
            if let Some((uid, _)) = &signal.event {
                live_events.insert(uid.clone());
                self.event_first_seen
                    .entry(uid.clone())
                    .or_insert(signal.observed_at);
            }
            groups.entry(signal.key.clone()).or_default().push(signal);
        }
        self.event_first_seen
            .retain(|uid, _| live_events.contains(uid));
        self.pending.retain(|key, pending| {
            groups.contains_key(key)
                || (key.detection.is_state()
                    && now.duration_since(pending.last_signal_at) <= STATE_GRACE)
        });

        let mut incidents = Vec::new();
        for (key, mut group) in groups {
            let first = group.iter().map(|s| s.observed_at).min();
            let last = group.iter().map(|s| s.observed_at).max();
            let (Some(first), Some(last)) = (first, last) else {
                continue;
            };
            let entry = self.pending.entry(key.clone()).or_insert(Pending {
                first_seen: first,
                last_seen: last,
                last_signal_at: now,
                pods: BTreeSet::new(),
                emitted: false,
            });
            entry.first_seen = entry.first_seen.min(first);
            entry.last_seen = entry.last_seen.max(last);
            entry.last_signal_at = now;
            entry
                .pods
                .extend(group.iter().filter_map(|s| s.pod.clone()));
            let (first_seen, last_seen, emitted) =
                (entry.first_seen, entry.last_seen, entry.emitted);
            let pods = entry.pods.clone();
            if emitted || !self.settled(&key, first_seen, last_seen, &group) {
                continue;
            }
            if let Some(entry) = self.pending.get_mut(&key) {
                entry.emitted = true;
            }
            group.sort_by(|a, b| {
                (&a.pod, &a.container, &a.reason, &a.message).cmp(&(
                    &b.pod,
                    &b.container,
                    &b.reason,
                    &b.message,
                ))
            });
            let sample = group.swap_remove(0);
            incidents.push(Incident {
                fingerprint: incident_fingerprint(&key, &sample, &pods),
                owner: sample.owner.clone(),
                key,
                first_seen,
                last_seen,
                pods,
                sample,
            });
        }
        incidents
    }

    fn settled(
        &self,
        key: &IncidentKey,
        first_seen: Timestamp,
        last_seen: Timestamp,
        group: &[Signal],
    ) -> bool {
        if !key.detection.settles() {
            return true;
        }
        if key.detection.is_state() {
            return last_seen.duration_since(first_seen) >= self.settle;
        }
        group.iter().any(|signal| {
            let Some((uid, series_first)) = &signal.event else {
                return false;
            };
            let seen_first = self.event_first_seen.get(uid).copied();
            let start = match (seen_first, *series_first) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => a.or(b).unwrap_or(signal.observed_at),
            };
            signal.observed_at.duration_since(start) >= self.settle
        })
    }
}

/// The fingerprint of an incident. The text it hashes is the sample's normalized message, except
/// for an image that cannot be pulled, whose message alternates between `ErrImagePull` and
/// `ImagePullBackOff`: there it is the image reference. For a workload that cannot create Pods,
/// the names of the Pods its controller tried to create count as the incident's Pods too.
fn incident_fingerprint(key: &IncidentKey, sample: &Signal, pods: &BTreeSet<String>) -> String {
    let mut names: Vec<&str> = pods.iter().map(String::as_str).collect();
    if key.detection == Detection::PodCreate {
        names.extend(generated_names(&sample.message, &key.owner_name));
    }
    let text = if key.detection == Detection::ImagePull {
        sample.image.clone().unwrap_or_default()
    } else {
        normalize(&sample.message, &names)
    };
    fingerprint(
        key.detection.id(),
        &key.namespace,
        &key.owner_kind,
        &key.owner_name,
        &text,
    )
}

/// The names in `message` that start with `owner` and a hyphen: the Pods a controller tried to
/// create and the API server refused (`D011`). Their suffix is generated (a ReplicaSet's or a
/// Job's Pods) or an ordinal (a StatefulSet's), and the Pod never existed, so it is no part of
/// what failed. A name is a run of the characters of a Kubernetes name, `[a-z0-9.-]`.
fn generated_names<'a>(message: &'a str, owner: &str) -> Vec<&'a str> {
    if owner.is_empty() {
        return Vec::new();
    }
    let prefix = format!("{owner}-");
    message
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'))
        .map(|word| word.trim_end_matches(['.', '-']))
        .filter(|word| word.len() > prefix.len() && word.starts_with(&prefix))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explain::reads_logs;
    use crate::record::{ContainerRecord, MAX_TEXT_BYTES, Regarding};
    use serde_json::{Value, json};

    fn t(seconds: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + seconds).expect("a valid timestamp")
    }

    fn owner(kind: &str, name: &str) -> OwnerRef {
        OwnerRef {
            api_version: "apps/v1".into(),
            kind: kind.into(),
            name: name.into(),
            uid: format!("uid-{name}"),
        }
    }

    fn pod(name: &str, phase: &str, waiting: Option<&str>) -> PodRecord {
        PodRecord {
            namespace: "shop".into(),
            name: name.into(),
            uid: format!("uid-{name}"),
            controller: None,
            phase: Some(phase.into()),
            unschedulable: None,
            unschedulable_reason: None,
            reason: None,
            message: None,
            failed_at: None,
            node_name: None,
            service_account: None,
            containers: vec![ContainerRecord {
                name: "api".into(),
                image: Some("registry.example.com/shop/api:1.4.2".into()),
                waiting_reason: waiting.map(Into::into),
                waiting_message: waiting.map(|_| format!("back-off restarting {name}")),
                last_termination_exit_code: Some(1),
                ..Default::default()
            }],
        }
    }

    fn event(uid: &str, reason: &str, kind: &str, name: &str, last: Timestamp) -> EventRecord {
        EventRecord {
            uid: uid.into(),
            reason: reason.into(),
            note: Some(format!("{reason} for {name}")),
            regarding: Regarding {
                api_version: if kind == "Job" { "batch/v1" } else { "v1" }.into(),
                kind: kind.into(),
                namespace: "shop".into(),
                name: name.into(),
                uid: None,
            },
            reporting_controller: None,
            first_seen: Some(last),
            last_seen: Some(last),
            count: 1,
        }
    }

    const SETTLE: SignedDuration = SignedDuration::from_secs(60);
    const FRESH: SignedDuration = SignedDuration::from_secs(600);

    /// Runs one check per entry, at its time and with its Pods, and returns how many incidents
    /// each check produced.
    fn checks(
        tracker: &mut Tracker,
        timeline: &[(i64, Vec<PodRecord>)],
        deploy: &OwnerRef,
    ) -> Vec<usize> {
        timeline
            .iter()
            .map(|(at, records)| {
                let views: Vec<_> = records
                    .iter()
                    .map(|record| PodView {
                        record,
                        owner: deploy,
                    })
                    .collect();
                tracker
                    .tick(t(*at), collect_signals(&views, &[], t(*at), FRESH))
                    .len()
            })
            .collect()
    }

    #[test]
    fn crashing_replicas_are_one_incident_after_the_settle_window() {
        let deploy = owner("Deployment", "api");
        let pods = [
            pod("api-a", "Running", Some("CrashLoopBackOff")),
            pod("api-b", "Running", Some("CrashLoopBackOff")),
            pod("web-a", "Running", None),
        ];
        let views: Vec<_> = pods
            .iter()
            .map(|record| PodView {
                record,
                owner: &deploy,
            })
            .collect();
        let mut tracker = Tracker::new(SETTLE);
        assert!(
            tracker
                .tick(t(0), collect_signals(&views, &[], t(0), FRESH))
                .is_empty()
        );
        assert!(
            tracker
                .tick(t(30), collect_signals(&views, &[], t(30), FRESH))
                .is_empty()
        );
        let incidents = tracker.tick(t(60), collect_signals(&views, &[], t(60), FRESH));
        assert_eq!(incidents.len(), 1);
        let incident = &incidents[0];
        assert_eq!(incident.key.detection, Detection::CrashLoop);
        assert_eq!(incident.key.owner_name, "api");
        assert_eq!(incident.pods.iter().collect::<Vec<_>>(), ["api-a", "api-b"]);
        assert_eq!(incident.sample.pod.as_deref(), Some("api-a"));
        assert_eq!(incident.first_seen, t(0));
        assert!(
            tracker
                .tick(t(90), collect_signals(&views, &[], t(90), FRESH))
                .is_empty()
        );
    }

    #[test]
    fn a_crash_loop_that_runs_between_back_offs_is_one_incident() {
        // The kubelet restarts the container when each back-off ends: for a check it is running,
        // then it crashes and waits again, for up to five minutes.
        let deploy = owner("Deployment", "api");
        let waiting = vec![pod("api-a", "Running", Some("CrashLoopBackOff"))];
        let running = vec![pod("api-a", "Running", None)];
        let mut timeline = Vec::new();
        for cycle in 0..3 {
            let start = cycle * 305;
            for s in (0..300).step_by(5) {
                timeline.push((start + s, waiting.clone()));
            }
            timeline.push((start + 300, running.clone()));
        }
        let mut tracker = Tracker::new(SETTLE);
        let total: usize = checks(&mut tracker, &timeline, &deploy).iter().sum();
        assert_eq!(total, 1);
        // A settle window longer than one back-off still settles.
        let mut tracker = Tracker::new(SignedDuration::from_secs(600));
        let total: usize = checks(&mut tracker, &timeline, &deploy).iter().sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn replicas_out_of_step_all_count() {
        // Two replicas crash out of step: at each check only one of them is in back-off.
        let deploy = owner("Deployment", "api");
        let a = pod("api-a", "Running", Some("CrashLoopBackOff"));
        let b = pod("api-b", "Running", Some("CrashLoopBackOff"));
        let mut tracker = Tracker::new(SETTLE);
        let mut found = Vec::new();
        for (i, at) in (0..=60).step_by(5).enumerate() {
            let record = if i % 2 == 0 { &a } else { &b };
            let views = [PodView {
                record,
                owner: &deploy,
            }];
            found.extend(tracker.tick(t(at), collect_signals(&views, &[], t(at), FRESH)));
        }
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pods.iter().collect::<Vec<_>>(), ["api-a", "api-b"]);
    }

    #[test]
    fn a_failure_that_clears_for_good_is_forgotten_and_can_settle_again() {
        let deploy = owner("Deployment", "api");
        let failing = vec![pod("api-a", "Running", Some("ImagePullBackOff"))];
        let healthy = vec![pod("api-a", "Running", None)];
        let mut tracker = Tracker::new(SETTLE);
        let produced = checks(
            &mut tracker,
            &[
                (0, failing.clone()),
                (40, healthy.clone()),  // recovered before settling
                (400, healthy),         // gone for longer than the grace period
                (410, failing.clone()), // a new first sighting
                (460, failing.clone()),
                (470, failing),
            ],
            &deploy,
        );
        assert_eq!(produced, [0, 0, 0, 0, 0, 1]);
    }

    #[test]
    fn a_single_event_does_not_settle_but_a_repeated_one_does() {
        let deploy = owner("Deployment", "api");
        let pods = [pod("api-a", "Running", None)];
        let views: Vec<_> = pods
            .iter()
            .map(|record| PodView {
                record,
                owner: &deploy,
            })
            .collect();
        let mut tracker = Tracker::new(SETTLE);
        let check = |tracker: &mut Tracker, record: &EventRecord, now: i64| {
            let ev = [EventView {
                record,
                job_owner: None,
            }];
            tracker
                .tick(t(now), collect_signals(&views, &ev, t(now), FRESH))
                .len()
        };
        let once = event("e1", "Unhealthy", "Pod", "api-a", t(0));
        assert_eq!(check(&mut tracker, &once, 10), 0);
        assert_eq!(check(&mut tracker, &once, 300), 0);
        // The same event, updated by its series 65 seconds after kausa first saw it.
        let mut again = once.clone();
        again.last_seen = Some(t(65));
        again.count = 2;
        assert_eq!(check(&mut tracker, &again, 70), 1);
    }

    #[test]
    fn single_events_of_different_pods_do_not_add_up() {
        // A rolling update: each new Pod fails its readiness probe once while it starts.
        let deploy = owner("Deployment", "api");
        let pods = [pod("api-a", "Running", None), pod("api-b", "Running", None)];
        let views: Vec<_> = pods
            .iter()
            .map(|record| PodView {
                record,
                owner: &deploy,
            })
            .collect();
        let a = event("ea", "Unhealthy", "Pod", "api-a", t(0));
        let b = event("eb", "Unhealthy", "Pod", "api-b", t(70));
        let ev = [
            EventView {
                record: &a,
                job_owner: None,
            },
            EventView {
                record: &b,
                job_owner: None,
            },
        ];
        let mut tracker = Tracker::new(SETTLE);
        assert!(
            tracker
                .tick(t(75), collect_signals(&views, &ev, t(75), FRESH))
                .is_empty()
        );
    }

    #[test]
    fn an_event_whose_own_series_spans_the_window_settles_at_once() {
        // kausa starts while a probe has been failing for minutes.
        let deploy = owner("Deployment", "api");
        let pods = [pod("api-a", "Running", None)];
        let views: Vec<_> = pods
            .iter()
            .map(|record| PodView {
                record,
                owner: &deploy,
            })
            .collect();
        let mut long = event("e1", "Unhealthy", "Pod", "api-a", t(300));
        long.first_seen = Some(t(0));
        long.count = 30;
        let ev = [EventView {
            record: &long,
            job_owner: None,
        }];
        let mut tracker = Tracker::new(SETTLE);
        let incidents = tracker.tick(t(301), collect_signals(&views, &ev, t(301), FRESH));
        assert_eq!(incidents.len(), 1);
    }

    #[test]
    fn an_unschedulable_pod_settles_like_a_state() {
        let deploy = owner("Deployment", "api");
        let mut pending = pod("api-a", "Pending", None);
        pending.unschedulable = Some("0/3 nodes are available: 3 Insufficient memory.".into());
        let mut running = pending.clone();
        running.phase = Some("Running".into());
        let mut tracker = Tracker::new(SETTLE);
        let produced = checks(
            &mut tracker,
            &[
                (0, vec![pending.clone()]),
                (30, vec![pending.clone()]),
                (65, vec![pending]),
            ],
            &deploy,
        );
        assert_eq!(produced, [0, 0, 1]);
        // A running Pod is not unschedulable, whatever stale condition it carries.
        let views = [PodView {
            record: &running,
            owner: &deploy,
        }];
        assert!(collect_signals(&views, &[], t(0), FRESH).is_empty());
    }

    #[test]
    fn a_failed_job_is_an_incident_at_once_about_its_cronjob() {
        let cron = OwnerRef {
            api_version: "batch/v1".into(),
            kind: "CronJob".into(),
            name: "report".into(),
            uid: "uid-report".into(),
        };
        let failed = event("ej", "BackoffLimitExceeded", "Job", "report-29000000", t(0));
        let ev = [EventView {
            record: &failed,
            job_owner: Some(&cron),
        }];
        let mut tracker = Tracker::new(SETTLE);
        let incidents = tracker.tick(t(1), collect_signals(&[], &ev, t(1), FRESH));
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].key.owner_kind, "CronJob");
        assert_eq!(incidents[0].key.detection, Detection::JobFailed);
        assert!(incidents[0].pods.is_empty());
    }

    #[test]
    fn an_event_dated_in_the_future_counts_as_now() {
        let failed = event("ej", "BackoffLimitExceeded", "Job", "report-1", t(10_000));
        let ev = [EventView {
            record: &failed,
            job_owner: None,
        }];
        let signals = collect_signals(&[], &ev, t(0), FRESH);
        assert_eq!(signals[0].observed_at, t(0));
    }

    #[test]
    fn the_fingerprint_does_not_depend_on_the_pod_or_the_back_off() {
        let deploy = owner("Deployment", "api");
        let run = |name: &str, waiting: &str, message: &str| {
            let mut record = pod(name, "Running", Some(waiting));
            record.containers[0].waiting_message = Some(message.to_owned());
            let views = [PodView {
                record: &record,
                owner: &deploy,
            }];
            let mut tracker = Tracker::new(SignedDuration::ZERO);
            tracker.tick(t(0), collect_signals(&views, &[], t(0), FRESH))[0]
                .fingerprint
                .clone()
        };
        assert_eq!(
            run(
                "api-1-a",
                "CrashLoopBackOff",
                "back-off 40s restarting failed container=api pod=api-1-a"
            ),
            run(
                "api-1-b",
                "CrashLoopBackOff",
                "back-off 5m0s restarting failed container=api pod=api-1-b"
            ),
        );
        // ErrImagePull and ImagePullBackOff are the same failure of the same image.
        assert_eq!(
            run("api-1-a", "ErrImagePull", "rpc error: code = NotFound"),
            run("api-1-a", "ImagePullBackOff", "Back-off pulling image"),
        );
    }

    // The shapes below follow what the kubelet and the scheduler wrote on kind (Kubernetes 1.37,
    // 2026-10-28); names, quantities and identifiers are invented.

    /// The kubelet's message when a Pod's containers outgrow their `ephemeral-storage` limits.
    const LIMIT: &str =
        "Pod ephemeral local storage usage exceeds the total limit of containers 50Mi. ";
    /// The kubelet's message when an `emptyDir` outgrows its `sizeLimit`.
    const EMPTY_DIR: &str = "Usage of EmptyDir volume \"scratch\" exceeds the limit \"20Mi\". ";
    /// The kubelet's message when it evicts a Pod to relieve node pressure.
    const PRESSURE: &str = "The node was low on resource: ephemeral-storage. Threshold quantity: \
        14711Mi, available: 14523Mi. Container cache was using 48Ki, request is 0, has larger \
        consumption of ephemeral-storage. ";
    /// The kubelet's message when it refuses a new Pod on a node under pressure (also reason
    /// `Evicted`): a consequence of the node's state, not of the workload.
    const REJECTED: &str = "Pod was rejected: The node had condition: [DiskPressure]. ";
    /// The kubelet's message for a Pod past its `activeDeadlineSeconds`.
    const DEADLINE: &str = "Pod was active on the node longer than the specified deadline";

    /// A Pod record reduced from a whole Pod, the way the watch delivers it.
    fn record_of(pod: Value) -> PodRecord {
        let pod = serde_json::from_value(pod).expect("the fixture is a valid Pod");
        PodRecord::from_pod(&pod).expect("a complete Pod")
    }

    /// A Pod of the Deployment `cache` that the kubelet failed at `t(at)` with `reason` and
    /// `message`: phase `Failed`, its container terminated `Error` with exit code 137 after
    /// `restarts` restarts, and `Ready` turned `False` for `PodFailed` at the same time.
    fn failed_pod(name: &str, reason: &str, message: &str, restarts: i32, at: i64) -> Value {
        let (started, at) = (t(at - 20).to_string(), t(at).to_string());
        let last_state = if restarts > 0 {
            json!({"terminated": {"reason": "Error", "exitCode": 1}})
        } else {
            json!({})
        };
        json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": name,
                "namespace": "shop",
                "uid": format!("uid-{name}"),
                "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet",
                                     "name": "cache-6b7f9d8c5d", "uid": "uid-cache-rs",
                                     "controller": true}]
            },
            "spec": {
                "nodeName": "worker-2",
                "containers": [{"name": "cache", "image": "busybox:1.37",
                                "resources": {"limits": {"ephemeral-storage": "50Mi"}}}]
            },
            "status": {
                "phase": "Failed",
                "reason": reason,
                "message": message,
                "conditions": [
                    {"type": "PodReadyToStartContainers", "status": "False",
                     "lastTransitionTime": at},
                    {"type": "Initialized", "status": "True", "lastTransitionTime": started},
                    {"type": "Ready", "status": "False", "reason": "PodFailed",
                     "lastTransitionTime": at},
                    {"type": "ContainersReady", "status": "False", "reason": "PodFailed",
                     "lastTransitionTime": at},
                    {"type": "PodScheduled", "status": "True", "lastTransitionTime": started}
                ],
                "containerStatuses": [{
                    "name": "cache", "image": "busybox:1.37", "imageID": "", "ready": false,
                    "restartCount": restarts,
                    "state": {"terminated": {"reason": "Error", "exitCode": 137,
                                             "startedAt": started, "finishedAt": at}},
                    "lastState": last_state
                }]
            }
        })
    }

    /// A Pod of the Deployment `db` still `Pending`, whose `PodScheduled` condition is `False`
    /// for `reason` with `message`.
    fn unscheduled_pod(name: &str, reason: &str, message: &str) -> Value {
        json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": name, "namespace": "shop", "uid": format!("uid-{name}")},
            "spec": {"containers": [{"name": "db", "image": "busybox:1.37"}]},
            "status": {
                "phase": "Pending",
                "conditions": [{"type": "PodScheduled", "status": "False", "reason": reason,
                                "message": message,
                                "lastTransitionTime": t(0).to_string()}]
            }
        })
    }

    /// The signals of `records`, all correlated to the Deployment `cache`, at `now`.
    fn state_signals(records: &[PodRecord], now: Timestamp) -> Vec<Signal> {
        let deploy = owner("Deployment", "cache");
        let views: Vec<_> = records
            .iter()
            .map(|record| PodView {
                record,
                owner: &deploy,
            })
            .collect();
        collect_signals(&views, &[], now, FRESH)
    }

    fn ids(signals: &[Signal]) -> Vec<&'static str> {
        signals.iter().map(|s| s.key.detection.id()).collect()
    }

    #[test]
    fn a_failed_pod_is_never_a_crash_loop() {
        // On kind, a container that had restarted once and was then evicted for its
        // ephemeral-storage limit was reported as D001 with exit code 137, for every replacement
        // Pod, and the incident never cleared. A Pod past its deadline after a restart has the
        // same container state.
        for (reason, message) in [("Evicted", LIMIT), ("DeadlineExceeded", DEADLINE)] {
            let failed = record_of(failed_pod("cache-a", reason, message, 1, -30));
            let found = ids(&state_signals(&[failed], t(0)));
            assert!(!found.contains(&"D001"), "{reason}: {found:?}");
        }
        // The same container state in a Pod that still runs is the crash loop.
        let mut running = failed_pod("cache-a", "Evicted", LIMIT, 1, -30);
        running["status"]["phase"] = json!("Running");
        let status = running["status"].as_object_mut().expect("a status");
        status.remove("reason");
        status.remove("message");
        assert_eq!(ids(&state_signals(&[record_of(running)], t(0))), ["D001"]);
        // A Job's Pod that failed for good with an out-of-memory kill is still D003.
        let mut oom = failed_pod("report-a", "Evicted", LIMIT, 0, -30);
        let status = oom["status"].as_object_mut().expect("a status");
        status.remove("reason");
        status.remove("message");
        oom["status"]["containerStatuses"][0]["state"]["terminated"]["reason"] = json!("OOMKilled");
        assert_eq!(ids(&state_signals(&[record_of(oom)], t(0))), ["D003"]);
    }

    #[test]
    fn an_evicted_pod_is_a_d009_state_that_reads_no_logs() {
        for message in [LIMIT, EMPTY_DIR, PRESSURE] {
            let evicted = record_of(failed_pod("cache-a", "Evicted", message, 0, -30));
            let signals = state_signals(&[evicted], t(0));
            assert_eq!(ids(&signals), ["D009"], "{message}");
            let signal = &signals[0];
            assert_eq!(signal.reason, "Evicted");
            assert!(
                signal.message.contains(message.trim()),
                "the status message is the signal's message: {:?}",
                signal.message
            );
            assert_eq!(signal.pod.as_deref(), Some("cache-a"));
            assert_eq!(signal.key.owner_name, "cache");
            // A state read from the Pod at every check; the evicted container's log is gone.
            assert!(signal.key.detection.is_state());
            assert!(!reads_logs(signal.key.detection));
        }
        // Not an eviction incident: the kubelet refusing a new Pod on a node under pressure (the
        // conditions are fresh, so only the message tells it apart), and a Pod past its deadline.
        let mut rejected = failed_pod("cache-b", "Evicted", REJECTED, 0, -30);
        let status = rejected["status"].as_object_mut().expect("a status");
        status.remove("containerStatuses");
        assert!(state_signals(&[record_of(rejected)], t(0)).is_empty());
        let deadline = record_of(failed_pod("cache-c", "DeadlineExceeded", DEADLINE, 0, -30));
        assert!(!ids(&state_signals(&[deadline], t(0))).contains(&"D009"));
    }

    #[test]
    fn only_a_recent_eviction_is_a_signal() {
        // Evicted Pods are not deleted: they stay in phase Failed for days. An old eviction must
        // not become an incident again each time kausa starts.
        let recent = record_of(failed_pod("cache-a", "Evicted", LIMIT, 0, -30));
        assert_eq!(ids(&state_signals(&[recent], t(0))), ["D009"]);
        let three_days = 3 * 24 * 3600;
        let old = record_of(failed_pod("cache-a", "Evicted", LIMIT, 0, -three_days));
        assert!(state_signals(&[old], t(0)).is_empty());
    }

    #[test]
    fn evictions_of_one_workload_are_one_incident() {
        // The ReplicaSet replaces each evicted Pod and the replacement is evicted again, every
        // 15 to 20 seconds; these containers had restarted once first.
        let pods: Vec<_> = [("cache-a", 0), ("cache-b", 20), ("cache-c", 40)]
            .into_iter()
            .map(|(name, at)| record_of(failed_pod(name, "Evicted", LIMIT, 1, at)))
            .collect();
        let mut tracker = Tracker::new(SETTLE);
        let mut found = Vec::new();
        for at in [40, 75, 110] {
            found.extend(tracker.tick(t(at), state_signals(&pods, t(at))));
        }
        let found_ids: Vec<_> = found.iter().map(|i| i.key.detection.id()).collect();
        assert_eq!(found_ids, ["D009"]);
        assert_eq!(
            found[0].pods.iter().collect::<Vec<_>>(),
            ["cache-a", "cache-b", "cache-c"]
        );
        assert_eq!(found[0].key.owner_name, "cache");
    }

    #[test]
    fn the_eviction_message_is_bounded() {
        let long = format!("{LIMIT}{}", "x".repeat(4 * MAX_TEXT_BYTES));
        let evicted = record_of(failed_pod("cache-a", "Evicted", &long, 0, -30));
        let signals = state_signals(&[evicted], t(0));
        assert_eq!(ids(&signals), ["D009"]);
        assert!(signals[0].message.len() <= MAX_TEXT_BYTES);
    }

    #[test]
    fn a_pod_the_scheduler_failed_on_is_unschedulable() {
        // A claim the scheduler cannot bind in PreBind (a ReadWriteMany claim on a
        // WaitForFirstConsumer class): the Pod stayed Pending for 25 minutes with this condition.
        let message =
            "running PreBind plugin \"VolumeBinding\": binding volumes: context deadline exceeded";
        let pending = record_of(unscheduled_pod("db-a", "SchedulerError", message));
        let signals = state_signals(std::slice::from_ref(&pending), t(0));
        assert_eq!(ids(&signals), ["D005"]);
        assert_eq!(signals[0].reason, "SchedulerError");
        assert_eq!(signals[0].message, message);
        // It settles like a state.
        let deploy = owner("Deployment", "db");
        let mut tracker = Tracker::new(SETTLE);
        let timeline = [
            (0, vec![pending.clone()]),
            (30, vec![pending.clone()]),
            (65, vec![pending]),
        ];
        assert_eq!(checks(&mut tracker, &timeline, &deploy), [0, 0, 1]);
        // A Pod held on purpose by its scheduling gates is not a failure.
        let gated = record_of(unscheduled_pod(
            "db-b",
            "SchedulingGated",
            "Scheduling is blocked due to non-empty scheduling gates",
        ));
        assert!(state_signals(&[gated], t(0)).is_empty());
    }

    #[test]
    fn a_sandbox_that_cannot_be_created_settles_like_an_event() {
        // A RuntimeClass whose handler the nodes do not have: the Pod waits in ContainerCreating,
        // which matches no state, and the kubelet keeps updating one Warning event.
        let deploy = owner("Deployment", "api");
        let pods = [pod("api-a", "Pending", Some("ContainerCreating"))];
        let views: Vec<_> = pods
            .iter()
            .map(|record| PodView {
                record,
                owner: &deploy,
            })
            .collect();
        let mut once = event("es", "FailedCreatePodSandBox", "Pod", "api-a", t(0));
        once.reporting_controller = Some("kubelet".into());
        once.note = Some(format!(
            "Failed to create pod sandbox: rpc error: code = Unknown desc = unable to get OCI \
             runtime for sandbox \"{}\": no runtime for \"no-such-handler\" is configured",
            "0123456789abcdef".repeat(4)
        ));
        let check = |tracker: &mut Tracker, record: &EventRecord, now: i64| {
            let ev = [EventView {
                record,
                job_owner: None,
            }];
            tracker.tick(t(now), collect_signals(&views, &ev, t(now), FRESH))
        };
        let ev = [EventView {
            record: &once,
            job_owner: None,
        }];
        let signals = collect_signals(&views, &ev, t(10), FRESH);
        assert_eq!(ids(&signals), ["D010"]);
        let detection = signals[0].key.detection;
        assert!(detection.settles());
        assert!(!detection.is_state());
        assert!(!reads_logs(detection));
        // One event does not settle; the same event updated a settle window later does.
        let mut tracker = Tracker::new(SETTLE);
        assert!(check(&mut tracker, &once, 10).is_empty());
        let mut again = once.clone();
        again.last_seen = Some(t(65));
        again.count = 2;
        let incidents = check(&mut tracker, &again, 70);
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].key.detection.id(), "D010");
        assert_eq!(incidents[0].key.owner_name, "api");
        assert_eq!(incidents[0].pods.iter().collect::<Vec<_>>(), ["api-a"]);
    }

    #[test]
    fn the_age_bound_of_an_eviction_is_the_event_freshness() {
        // A failure time exactly at the freshness is still recent, one second older is not, and a
        // failure time ahead of kausa's clock counts as now.
        let at = |seconds: i64| record_of(failed_pod("cache-a", "Evicted", LIMIT, 0, seconds));
        let fresh = FRESH.as_secs();
        assert_eq!(ids(&state_signals(&[at(-fresh)], t(0))), ["D009"]);
        assert!(state_signals(&[at(-fresh - 1)], t(0)).is_empty());
        assert_eq!(ids(&state_signals(&[at(120)], t(0))), ["D009"]);
    }

    #[test]
    fn an_eviction_with_no_failure_time_is_no_signal() {
        // The failure time comes from the Ready condition's last transition or a container's
        // finish time. With neither, kausa cannot tell a new eviction from one days old.
        let mut timeless = failed_pod("cache-a", "Evicted", LIMIT, 0, -30);
        let status = timeless["status"].as_object_mut().expect("a status");
        status.remove("conditions");
        status.remove("containerStatuses");
        assert!(state_signals(&[record_of(timeless)], t(0)).is_empty());
        // A container the kubelet could no longer find has no finish time; the Ready condition's
        // last transition is enough.
        let mut unknown = failed_pod("cache-a", "Evicted", PRESSURE, 0, -30);
        let terminated = &mut unknown["status"]["containerStatuses"][0]["state"]["terminated"];
        terminated["reason"] = json!("ContainerStatusUnknown");
        terminated
            .as_object_mut()
            .expect("a terminated state")
            .remove("finishedAt");
        assert_eq!(ids(&state_signals(&[record_of(unknown)], t(0))), ["D009"]);
    }

    #[test]
    fn rejected_pods_do_not_join_the_eviction_incident() {
        // On a node under pressure the kubelet evicts one Pod and then refuses its replacements
        // (also reason Evicted); the incident counts only the eviction.
        let mut pods = vec![record_of(failed_pod("cache-a", "Evicted", PRESSURE, 0, 0))];
        for name in ["cache-b", "cache-c", "cache-d"] {
            let mut rejected = failed_pod(name, "Evicted", REJECTED, 0, 5);
            let status = rejected["status"].as_object_mut().expect("a status");
            status.remove("containerStatuses");
            pods.push(record_of(rejected));
        }
        let mut tracker = Tracker::new(SETTLE);
        let mut found = Vec::new();
        for at in [5, 40, 70] {
            found.extend(tracker.tick(t(at), state_signals(&pods, t(at))));
        }
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key.detection.id(), "D009");
        assert_eq!(found[0].pods.iter().collect::<Vec<_>>(), ["cache-a"]);
    }

    #[test]
    fn an_eviction_is_explained_once_and_not_again_after_a_restart() {
        // One eviction: it settles once while it is recent, and a fresh process (a new tracker)
        // that starts after the freshness raises nothing for the same Pod.
        let evicted = record_of(failed_pod("cache-a", "Evicted", LIMIT, 0, 0));
        let deploy = owner("Deployment", "cache");
        let mut tracker = Tracker::new(SETTLE);
        let timeline: Vec<_> = [0, 65, 300, 600, 700, 1000]
            .into_iter()
            .map(|at| (at, vec![evicted.clone()]))
            .collect();
        assert_eq!(checks(&mut tracker, &timeline, &deploy), [0, 1, 0, 0, 0, 0]);
        let mut restarted = Tracker::new(SETTLE);
        let later: Vec<_> = [601, 700]
            .into_iter()
            .map(|at| (at, vec![evicted.clone()]))
            .collect();
        assert_eq!(checks(&mut restarted, &later, &deploy), [0, 0]);
    }

    #[test]
    fn a_scheduler_error_counts_only_while_the_pod_is_pending() {
        let message =
            "running PreBind plugin \"VolumeBinding\": binding volumes: context deadline exceeded";
        let mut stale = unscheduled_pod("db-a", "SchedulerError", message);
        stale["status"]["phase"] = json!("Running");
        assert!(state_signals(&[record_of(stale)], t(0)).is_empty());
    }

    // Acceptance tests for `D011`, `D012` and `D013`, written before the code. Their events are
    // about the object that fails, never about a Pod: `collect_signals` keys them on the top-level
    // owner the event view carries (the Deployment of a ReplicaSet and the CronJob of a Job, which
    // `run.rs` resolves through `owner.rs`, as it does for `D008`) or, without one, on the object
    // itself, and needs no Pod. The shapes follow what the controllers wrote on kind (Kubernetes
    // 1.37, 2026-10-28); names and numbers are invented.

    /// A Warning event about the object `(api_version, kind, name)` in `shop`, whose series ran
    /// from `first` to `last` with `count` occurrences.
    fn event_about(
        uid: &str,
        (api_version, kind, name): (&str, &str, &str),
        reason: &str,
        note: &str,
        (first, last, count): (Timestamp, Timestamp, i32),
    ) -> EventRecord {
        EventRecord {
            uid: uid.into(),
            reason: reason.into(),
            note: Some(note.into()),
            regarding: Regarding {
                api_version: api_version.into(),
                kind: kind.into(),
                namespace: "shop".into(),
                name: name.into(),
                uid: Some(format!("uid-{name}")),
            },
            reporting_controller: None,
            first_seen: Some(first),
            last_seen: Some(last),
            count,
        }
    }

    /// The views of events, each with the top-level owner it was resolved to, if any.
    fn views(events: &[(EventRecord, Option<OwnerRef>)]) -> Vec<EventView<'_>> {
        events
            .iter()
            .map(|(record, owner)| EventView {
                record,
                job_owner: owner.as_ref(),
            })
            .collect()
    }

    /// The API server's refusal of a Pod by a ResourceQuota, as the controllers write it.
    fn quota_refusal(pod: &str) -> String {
        format!(
            "Error creating: pods \"{pod}\" is forbidden: exceeded quota: compute-budget, \
             requested: pods=1, used: pods=3, limited: pods=3"
        )
    }

    const COMBINED: &str = "(combined from similar events): ";
    const REPLICASET: (&str, &str, &str) = ("apps/v1", "ReplicaSet", "checkout-7fffb9bf5c");
    const CLAIM: (&str, &str, &str) = ("v1", "PersistentVolumeClaim", "orders-data");
    const NO_CLASS: &str = r#"storageclass.storage.k8s.io "fast-ssd" not found"#;

    fn cronjob(name: &str) -> OwnerRef {
        OwnerRef {
            api_version: "batch/v1".into(),
            kind: "CronJob".into(),
            name: name.into(),
            uid: format!("uid-{name}"),
        }
    }

    #[test]
    fn a_replicaset_that_cannot_create_pods_is_one_incident_about_its_deployment() {
        let deploy = owner("Deployment", "checkout");
        // The first refusals are separate events, one per generated Pod name: they never add up.
        let mut events: Vec<_> = [(0, "d98v6"), (2, "7frqt"), (4, "k2m4p"), (6, "x8w2n")]
            .into_iter()
            .map(|(at, suffix)| {
                let note = quota_refusal(&format!("checkout-7fffb9bf5c-{suffix}"));
                let event = event_about(
                    &format!("e-{suffix}"),
                    REPLICASET,
                    "FailedCreate",
                    &note,
                    (t(at), t(at), 1),
                );
                (event, Some(deploy.clone()))
            })
            .collect();
        let mut tracker = Tracker::new(SETTLE);
        let mut check = |events: &[(EventRecord, Option<OwnerRef>)], now: i64| {
            tracker.tick(t(now), collect_signals(&[], &views(events), t(now), FRESH))
        };
        assert!(check(&events, 10).is_empty());
        // Then client-go folds them into one event that it keeps updating; it settles once its
        // series spans the settle window.
        let combined = |last: i64, count: i32| {
            let note = format!("{COMBINED}{}", quota_refusal("checkout-7fffb9bf5c-b6n3s"));
            let event = event_about(
                "e-combined",
                REPLICASET,
                "FailedCreate",
                &note,
                (t(20), t(last), count),
            );
            (event, Some(deploy.clone()))
        };
        events.push(combined(20, 2));
        assert!(check(&events, 20).is_empty());
        events.pop();
        events.push(combined(85, 9));
        let incidents = check(&events, 90);
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        let incident = &incidents[0];
        assert_eq!(incident.key.detection.id(), "D011");
        assert_eq!(
            (
                incident.key.owner_kind.as_str(),
                incident.key.owner_name.as_str()
            ),
            ("Deployment", "checkout")
        );
        assert_eq!(incident.owner, deploy);
        assert!(incident.pods.is_empty());
        assert_eq!(incident.sample.reason, "FailedCreate");
        assert_eq!(incident.sample.pod, None);
        // Once a settle window has passed, the incident is not raised again while it lasts.
        assert!(check(&events, 95).is_empty());
    }

    #[test]
    fn an_event_about_a_controller_names_its_owner_or_the_controller_itself() {
        let deploy = owner("Deployment", "checkout");
        let statefulset = "Create Pod db-0 in StatefulSet db failed error: pods \"db-0\" is \
                           forbidden: exceeded quota: compute-budget, requested: pods=1, used: \
                           pods=3, limited: pods=3";
        let pod_security = "Error creating: pods \"node-agent-x7k2p\" is forbidden: violates \
                            PodSecurity \"restricted:latest\": privileged (container \"collector\" \
                            must not set securityContext.privileged=true)";
        let cases = [
            // A ReplicaSet resolved to its Deployment, and one whose lookup failed.
            (
                REPLICASET,
                quota_refusal("checkout-7fffb9bf5c-d98v6"),
                Some(deploy.clone()),
                ("apps/v1", "Deployment", "checkout"),
            ),
            (
                REPLICASET,
                quota_refusal("checkout-7fffb9bf5c-d98v6"),
                None,
                ("apps/v1", "ReplicaSet", "checkout-7fffb9bf5c"),
            ),
            // A Job resolved to its CronJob.
            (
                ("batch/v1", "Job", "report-29000000"),
                quota_refusal("report-29000000-m4q9z"),
                Some(cronjob("report")),
                ("batch/v1", "CronJob", "report"),
            ),
            // StatefulSets and DaemonSets are top-level owners, named from the event alone.
            (
                ("apps/v1", "StatefulSet", "db"),
                statefulset.to_owned(),
                None,
                ("apps/v1", "StatefulSet", "db"),
            ),
            (
                ("apps/v1", "DaemonSet", "node-agent"),
                pod_security.to_owned(),
                None,
                ("apps/v1", "DaemonSet", "node-agent"),
            ),
        ];
        for (about, note, resolved, (api_version, kind, name)) in cases {
            let events = [(
                event_about("e1", about, "FailedCreate", &note, (t(0), t(0), 1)),
                resolved,
            )];
            let signals = collect_signals(&[], &views(&events), t(1), FRESH);
            assert_eq!(ids(&signals), ["D011"], "{about:?}");
            let signal = &signals[0];
            assert_eq!(
                (
                    signal.key.owner_kind.as_str(),
                    signal.key.owner_name.as_str()
                ),
                (kind, name),
                "{about:?}"
            );
            assert_eq!(signal.owner.api_version, api_version, "{about:?}");
            assert_eq!(signal.owner.uid, format!("uid-{name}"), "{about:?}");
            assert_eq!(signal.pod, None);
            assert_eq!(signal.message, note);
        }
    }

    #[test]
    fn the_d011_fingerprint_does_not_depend_on_the_generated_pod_name() {
        // Each refusal names the Pod the controller tried to create, a name the API server
        // generated and nobody will see again; the aggregated event adds the prefix. The same
        // refusal must keep one fingerprint, so that an incident that clears while the
        // controller backs off and comes back reuses its explanation.
        let fingerprint = |about, owner: Option<OwnerRef>, note: &str| {
            let events = [(
                event_about("e1", about, "FailedCreate", note, (t(0), t(0), 1)),
                owner,
            )];
            let mut tracker = Tracker::new(SignedDuration::ZERO);
            tracker
                .tick(t(0), collect_signals(&[], &views(&events), t(0), FRESH))
                .first()
                .expect("a D011 incident")
                .fingerprint
                .clone()
        };
        let deploy = Some(owner("Deployment", "checkout"));
        let job = ("batch/v1", "Job", "report-29000000");
        let daemonset = ("apps/v1", "DaemonSet", "node-agent");
        for (about, owner, a, b) in [
            (
                REPLICASET,
                deploy.clone(),
                "checkout-7fffb9bf5c-d98v6",
                "checkout-7fffb9bf5c-7frqt",
            ),
            (
                job,
                Some(cronjob("report")),
                "report-29000000-x7k2p",
                "report-29000000-m4q9z",
            ),
            (daemonset, None, "node-agent-x7k2p", "node-agent-b2c4d"),
        ] {
            assert_eq!(
                fingerprint(about, owner.clone(), &quota_refusal(a)),
                fingerprint(about, owner, &format!("{COMBINED}{}", quota_refusal(b))),
                "{about:?}"
            );
        }
        // A different refusal of the same workload is a different failure.
        let pod_security = "Error creating: pods \"checkout-7fffb9bf5c-d98v6\" is forbidden: \
                            violates PodSecurity \"restricted:latest\": privileged (container \
                            \"api\" must not set securityContext.privileged=true)";
        assert_ne!(
            fingerprint(
                REPLICASET,
                deploy.clone(),
                &quota_refusal("checkout-7fffb9bf5c-d98v6")
            ),
            fingerprint(REPLICASET, deploy, pod_security)
        );
    }

    #[test]
    fn a_claim_that_cannot_be_provisioned_is_an_incident_about_the_claim() {
        // A claim whose StorageClass does not exist: the PersistentVolume controller updates one
        // event about every 15 seconds. No Pod has to use the claim.
        let mut tracker = Tracker::new(SETTLE);
        let mut check = |last: i64, count: i32| {
            let events = [(
                event_about(
                    "e1",
                    CLAIM,
                    "ProvisioningFailed",
                    NO_CLASS,
                    (t(0), t(last), count),
                ),
                None,
            )];
            tracker.tick(
                t(last + 1),
                collect_signals(&[], &views(&events), t(last + 1), FRESH),
            )
        };
        assert!(check(0, 1).is_empty());
        let incidents = check(75, 6);
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        let incident = &incidents[0];
        assert_eq!(incident.key.detection.id(), "D012");
        assert_eq!(
            (
                incident.key.owner_kind.as_str(),
                incident.key.owner_name.as_str()
            ),
            ("PersistentVolumeClaim", "orders-data")
        );
        assert_eq!(
            (
                incident.owner.api_version.as_str(),
                incident.owner.uid.as_str()
            ),
            ("v1", "uid-orders-data")
        );
        assert!(incident.pods.is_empty());
        // An external provisioner that backs off stops updating its event while the claim stays
        // Pending: after the freshness the event is no signal, like any other.
        let silent = [(
            event_about(
                "e2",
                CLAIM,
                "ProvisioningFailed",
                "failed to provision volume with StorageClass \"fast-ssd\": NodePath only \
                 supports ReadWriteOnce and ReadWriteOncePod (1.22+) access modes",
                (t(0), t(0), 8),
            ),
            None,
        )];
        assert!(collect_signals(&[], &views(&silent), t(601), FRESH).is_empty());
    }

    #[test]
    fn an_unbound_claim_explains_the_claim_and_the_workload_apart() {
        // A Deployment whose Pod mounts a claim with an unknown StorageClass: the Pod cannot be
        // scheduled (D005, on the Deployment) and the claim cannot be provisioned (D012, on the
        // claim). They are two incidents about two objects, each Event on the object it explains:
        // kausa does not record which claims a Pod mounts, and an external provisioner's event
        // goes quiet while the Pod's condition holds (docs/reference/rules.md).
        let deploy = owner("Deployment", "orders");
        let mut pending = pod("orders-7c9d5b8f6-q4w2e", "Pending", None);
        pending.unschedulable = Some(
            "0/3 nodes are available: pod has unbound immediate PersistentVolumeClaims. \
             preemption: 0/3 nodes are available: 3 Preemption is not helpful for scheduling."
                .into(),
        );
        let pods = [PodView {
            record: &pending,
            owner: &deploy,
        }];
        let mut tracker = Tracker::new(SETTLE);
        let mut produced = Vec::new();
        for (at, count) in [(0, 1), (30, 3), (65, 5)] {
            let events = [(
                event_about(
                    "e1",
                    CLAIM,
                    "ProvisioningFailed",
                    NO_CLASS,
                    (t(0), t(at), count),
                ),
                None,
            )];
            produced
                .extend(tracker.tick(t(at), collect_signals(&pods, &views(&events), t(at), FRESH)));
        }
        let mut keys: Vec<_> = produced
            .iter()
            .map(|i| {
                (
                    i.key.detection.id(),
                    i.key.owner_kind.as_str(),
                    i.key.owner_name.as_str(),
                )
            })
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                ("D005", "Deployment", "orders"),
                ("D012", "PersistentVolumeClaim", "orders-data")
            ]
        );
    }

    #[test]
    fn an_autoscaler_that_cannot_get_metrics_is_one_incident_per_autoscaler() {
        // No metrics API: the controller updates FailedGetResourceMetric about every 15 seconds,
        // and stops updating FailedComputeMetricsReplicas after about three minutes while the
        // failure goes on. An autoscaler whose target does not exist gets FailedGetScale.
        let hpa = ("autoscaling/v2", "HorizontalPodAutoscaler", "checkout");
        let orphan = ("autoscaling/v2", "HorizontalPodAutoscaler", "orphan");
        let cpu = "failed to get cpu utilization: unable to get metrics for resource cpu: unable \
                   to fetch metrics from resource metrics API: the server could not find the \
                   requested resource (get pods.metrics.k8s.io)";
        let replicas = format!(
            "invalid metrics (1 invalid out of 1), first error is: failed to get cpu resource \
             metric value: {cpu}"
        );
        let at = |metric_last: i64, metric_count: i32| {
            [
                (
                    event_about(
                        "e-metric",
                        hpa,
                        "FailedGetResourceMetric",
                        cpu,
                        (t(0), t(metric_last), metric_count),
                    ),
                    None,
                ),
                (
                    event_about(
                        "e-replicas",
                        hpa,
                        "FailedComputeMetricsReplicas",
                        &replicas,
                        (t(0), t(180), 12),
                    ),
                    None,
                ),
                (
                    event_about(
                        "e-scale",
                        orphan,
                        "FailedGetScale",
                        r#"deployments.apps "storefront" not found"#,
                        (t(metric_last - 300), t(metric_last), 21),
                    ),
                    None,
                ),
            ]
        };
        // Early on, both reasons of one autoscaler are one incident.
        let early = at(195, 14);
        let mut tracker = Tracker::new(SETTLE);
        let incidents = tracker.tick(t(200), collect_signals(&[], &views(&early), t(200), FRESH));
        let mut owners: Vec<_> = incidents
            .iter()
            .map(|i| (i.key.detection.id(), i.key.owner_name.as_str()))
            .collect();
        owners.sort_unstable();
        assert_eq!(owners, [("D013", "checkout"), ("D013", "orphan")]);
        for incident in &incidents {
            assert_eq!(incident.key.owner_kind, "HorizontalPodAutoscaler");
            assert_eq!(incident.owner.api_version, "autoscaling/v2");
            assert!(incident.pods.is_empty());
        }
        // Later, the event that keeps repeating carries the failure alone.
        let late = at(1200, 81);
        let signals = collect_signals(&[], &views(&late), t(1205), FRESH);
        let mut reasons: Vec<_> = signals.iter().map(|s| s.reason.as_str()).collect();
        reasons.sort_unstable();
        assert_eq!(reasons, ["FailedGetResourceMetric", "FailedGetScale"]);
        assert_eq!(ids(&signals), ["D013", "D013"]);
    }
}
