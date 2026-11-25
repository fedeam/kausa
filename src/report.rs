//! The fact path (docs/adr/0027-fact-statement-for-the-detections.md, points 3 and 5): a settled
//! incident's facts go on a bounded queue, and one slot of its own writes a `Detected` Event for
//! each, unless `--no-events`, and then queues its `detected` message when a webhook is set,
//! whatever the model, the budget and the breaker do.

use std::collections::VecDeque;
use std::io;

use k8s_openapi::jiff::Timestamp;
use serde_json::json;

use crate::events::{Emitted, Writer};
use crate::fact;
use crate::incident::Incident;
use crate::leader::Term;
use crate::log;
use crate::metrics::Metrics;
use crate::notify::{self, Outbox};
use crate::owner::{Hop, is_dns_subdomain};
use crate::record::{DeploymentRecord, OwnerRef, PodRecord, Regarding, Uid};

/// The most facts that wait for the fact slot. A fact pushed on a full queue drops the oldest.
pub const FACT_QUEUE_CAPACITY: usize = 1024;

/// The kind of a sample Pod's controller, as the Incident names it: any kind but these is `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControllerKind {
    ReplicaSet,
    Job,
    StatefulSet,
    DaemonSet,
    Other,
}

impl ControllerKind {
    pub(crate) const ALL: [ControllerKind; 5] = [
        ControllerKind::ReplicaSet,
        ControllerKind::Job,
        ControllerKind::StatefulSet,
        ControllerKind::DaemonSet,
        ControllerKind::Other,
    ];

    /// The kind of an `ownerReference` whose kind is `kind`.
    pub(crate) fn of(kind: &str) -> ControllerKind {
        Self::ALL
            .into_iter()
            .find(|known| *known != ControllerKind::Other && known.as_str() == kind)
            .unwrap_or(ControllerKind::Other)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ControllerKind::ReplicaSet => "ReplicaSet",
            ControllerKind::Job => "Job",
            ControllerKind::StatefulSet => "StatefulSet",
            ControllerKind::DaemonSet => "DaemonSet",
            ControllerKind::Other => "Other",
        }
    }
}

/// The kinds of the object an Event incident is about, when that object is not a Pod.
pub(crate) const REGARDING_KINDS: [&str; 7] = [
    "Job",
    "ReplicaSet",
    "StatefulSet",
    "DaemonSet",
    "CronJob",
    "PersistentVolumeClaim",
    "HorizontalPodAutoscaler",
];

/// What the Incident of a settled fact needs beyond the fact
/// (docs/adr/0028-incident-resource-and-remediation-report.md, point 3), taken in the tick that
/// emits it: kept only with incidents on. Values from cluster objects are kept as checked types (a
/// UID as its 16 bytes, a kind from a closed list) or as names that are DNS-1123 subdomains; one
/// that does not fit is kept as a mark the Incident's check refuses, never as its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncidentData {
    /// The emitting tick's `now`; the Incident writes it to the second.
    pub settled_at: Timestamp,
    /// The owner lookup failed when kausa resolved the incident's owner: the sample Pod's, or the
    /// lookup of the object of an Event about a Job or a ReplicaSet.
    pub owner_lookup_failed: bool,
    pub about: About,
}

/// What an incident is about besides its owner. One of the three, never two, so the name of a
/// sample's ReplicaSet and the name of `regarding` share their memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum About {
    /// `D001` to `D007`, `D009` and `D010`: the sample Pod. Its UID is `None` when its text is not
    /// a UID.
    Sample {
        pod_uid: Option<Uid>,
        controller: Option<Controller>,
    },
    /// `D008`, `D011` to `D013`: the object the Event is about, when it is not a Pod. Its kind is
    /// `None` when it is not one of `REGARDING_KINDS`, its name `None` when it is not a DNS-1123
    /// subdomain.
    Regarding {
        kind: Option<&'static str>,
        name: Option<Box<str>>,
        uid: MaybeUid,
    },
    /// `D014`: the Deployment's generation and revision at detection, from its record, both `None`
    /// when kausa no longer holds it.
    Deployment {
        generation: Option<i64>,
        revision: Option<u64>,
    },
}

/// The sample Pod's controller, from the Pod's own controller `ownerReference`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Controller {
    pub kind: ControllerKind,
    /// `None` when the reference's UID is not a UID.
    pub uid: Option<Uid>,
    /// An `apps/v1` ReplicaSet's name, kept only for the fact slot's read of its revision and
    /// never written to the Incident: `None` for any other controller, and for a name the owner
    /// lookup would refuse, which then reads nothing.
    pub replica_set: Option<Box<str>>,
}

/// A UID an object may lack: its 16 bytes, or a mark that its text is not a UID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaybeUid {
    Absent,
    Uid(Uid),
    NotAUid,
}

impl MaybeUid {
    pub fn of(text: Option<&str>) -> MaybeUid {
        match text {
            None => MaybeUid::Absent,
            Some(text) => Uid::parse(text).map_or(MaybeUid::NotAUid, MaybeUid::Uid),
        }
    }
}

impl About {
    /// The sample Pod of `record`.
    pub fn sample(record: &PodRecord) -> About {
        About::Sample {
            pod_uid: Uid::parse(&record.uid),
            controller: record.controller.as_ref().map(Controller::of),
        }
    }

    /// The object an Event is about.
    pub fn regarding(regarding: &Regarding) -> About {
        About::Regarding {
            kind: REGARDING_KINDS
                .into_iter()
                .find(|kind| *kind == regarding.kind),
            name: is_dns_subdomain(&regarding.name).then(|| regarding.name.as_str().into()),
            uid: MaybeUid::of(regarding.uid.as_deref()),
        }
    }

    /// A `D014` Deployment, from its record when kausa holds it.
    pub fn deployment(record: Option<&DeploymentRecord>) -> About {
        About::Deployment {
            generation: record.and_then(|record| record.generation),
            revision: record.and_then(|record| record.revision),
        }
    }

    /// The bytes it holds on the heap.
    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        match self {
            About::Sample {
                controller:
                    Some(Controller {
                        replica_set: Some(name),
                        ..
                    }),
                ..
            }
            | About::Regarding {
                name: Some(name), ..
            } => name.len(),
            _ => 0,
        }
    }
}

impl Controller {
    /// The controller `owner` of a Pod.
    pub fn of(owner: &OwnerRef) -> Controller {
        Controller {
            kind: ControllerKind::of(&owner.kind),
            uid: Uid::parse(&owner.uid),
            replica_set: (Hop::of(owner) == Some(Hop::ReplicaSet) && is_dns_subdomain(&owner.name))
                .then(|| owner.name.as_str().into()),
        }
    }
}

/// What the fact slot needs of a settled incident: the facts its statement is rendered from, and
/// where its Event goes. Not the incident itself, whose Pod set and sample message have no bound
/// this small. The statement is rendered from `facts` when it is written, the same bytes as the
/// `incident` line's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub facts: fact::Facts,
    pub namespace: String,
    pub owner: OwnerRef,
    /// The sample's Pod, the Event's `related`.
    pub pod: Option<String>,
    pub fingerprint: String,
    pub first_seen: Timestamp,
    /// What its Incident needs, with incidents on: one pointer, `None` with incidents off.
    pub incident: Option<Box<IncidentData>>,
}

impl Fact {
    pub fn new(incident: &Incident, facts: fact::Facts) -> Self {
        Self {
            facts,
            namespace: incident.key.namespace.clone(),
            owner: incident.owner.clone(),
            pod: incident.sample.pod.clone(),
            fingerprint: incident.fingerprint.clone(),
            first_seen: incident.first_seen,
            incident: None,
        }
    }

    /// The fact statement, as `fact::statement` gave it for the incident.
    pub fn statement(&self) -> String {
        fact::render(&self.facts)
    }

    /// The bytes this entry asks of the allocator, with the entry itself: what a full queue is
    /// computed from.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.facts.container.as_ref().map_or(0, String::capacity)
            + self.namespace.capacity()
            + self.owner.api_version.capacity()
            + self.owner.kind.capacity()
            + self.owner.name.capacity()
            + self.owner.uid.capacity()
            + self.pod.as_ref().map_or(0, String::capacity)
            + self.fingerprint.capacity()
            + self.incident.as_ref().map_or(0, |data| {
                size_of::<IncidentData>() + data.about.heap_bytes()
            })
    }
}

/// The facts waiting for the fact slot, oldest first, at most `FACT_QUEUE_CAPACITY`.
#[derive(Default)]
pub struct Queue {
    entries: VecDeque<Fact>,
}

impl Queue {
    /// Adds `fact` at the back. On a full queue the oldest entry leaves, and is returned.
    pub fn push(&mut self, fact: Fact) -> Option<Fact> {
        let dropped = if self.entries.len() >= FACT_QUEUE_CAPACITY {
            self.entries.pop_front()
        } else {
            None
        };
        self.entries.push_back(fact);
        dropped
    }

    pub fn pop(&mut self) -> Option<Fact> {
        self.entries.pop_front()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The bytes the queue holds: its entries' and the ring's own.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Fact>()
            + self
                .entries
                .iter()
                .map(|fact| fact.bytes() - size_of::<Fact>())
                .sum::<usize>()
    }
}

/// Adds `fact` to `queue`; a fact dropped to make room is logged and counted, and owes the
/// webhook's `capped` message, since its `detected` message is never queued.
pub fn enqueue(
    queue: &mut Queue,
    fact: Fact,
    metrics: &Metrics,
    outbox: Option<&Outbox<'_>>,
) -> io::Result<()> {
    if let Some(dropped) = queue.push(fact) {
        if let Some(outbox) = outbox {
            outbox.owe_capped();
        }
        metrics.inc("kausa_facts_dropped_total", &[]);
        log::warn(
            "fact dropped",
            json!({"fingerprint": dropped.fingerprint, "reason": "queue full"}),
        )?;
    }
    Ok(())
}

/// What the fact slot works with, across terms: the Event writer, unless `--no-events`, and the
/// outbox, with `--notify-url-file`.
pub struct Reporter<'a> {
    pub writer: Option<Writer>,
    pub outbox: Option<&'a Outbox<'a>>,
    pub metrics: &'a Metrics,
    pub term: &'a Term,
}

impl Reporter<'_> {
    /// Writes the `Detected` Event of `fact`: one create, or the patch of its series. A failed
    /// write is logged and counted, and not tried again. Then, while the term holds, queues the
    /// fact's `detected` message, whether the write succeeded or not: the message never waits for
    /// another try.
    pub async fn report(&self, fact: Fact) -> io::Result<()> {
        if let Some(writer) = &self.writer {
            match writer
                .emit_detected(&fact, Timestamp::now(), self.term)
                .await
            {
                Ok(Emitted::Written) => {
                    self.metrics
                        .inc("kausa_events_written_total", &[("reason", "Detected")]);
                }
                Ok(Emitted::Skipped) => {}
                Err(error) => {
                    self.metrics.inc("kausa_event_write_errors_total", &[]);
                    log::warn(
                        "event write failed",
                        json!({"fingerprint": fact.fingerprint, "error": error}),
                    )?;
                }
            }
        }
        if let Some(outbox) = self.outbox
            && self.term.holds()
        {
            outbox.enqueue(notify::detected(&fact))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detection;
    use crate::fact::{LONGEST_STATEMENT_BYTES, MAX_STATEMENT_BYTES};

    const UID: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

    fn fact(i: usize) -> Fact {
        Fact {
            facts: fact::Facts {
                detection: Detection::CrashLoop,
                container: Some("app".into()),
                reason: Some("CrashLoopBackOff"),
                exit_code: Some(1),
                pods_seen: 1 + i % 99,
                pods_seen_is_minimum: false,
                pods_now: 100,
                revision: None,
            },
            namespace: "shop".into(),
            owner: OwnerRef {
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                name: format!("web-{i:04}"),
                uid: String::new(),
            },
            pod: Some(format!("web-{i:04}-0")),
            fingerprint: format!("{i:016x}"),
            first_seen: Timestamp::UNIX_EPOCH,
            incident: None,
        }
    }

    /// The `i`th entry with every name at the longest Kubernetes allows: a container name of 63
    /// bytes, a namespace of 63, an owner of a group of 253 bytes and a version and a kind of 63,
    /// an owner and a Pod named with 253 bytes, and a UID of 36; the longest statement: the
    /// detection with the longest name, its longest reason, the longest exit code and Pod counts
    /// of twenty digits; and, with incidents on, the Incident data that holds the most: a name of
    /// 253 bytes, a ReplicaSet's or `regarding`'s, the only name it keeps.
    fn longest(i: usize) -> Fact {
        // Each name holds exactly its bytes, as the clones `Fact::new` takes of a real incident's
        // names do.
        let exact = |text: String| text.into_boxed_str().into_string();
        let numbered = |length: usize| exact(format!("{i:04}{}", "a".repeat(length - 4)));
        Fact {
            facts: fact::Facts {
                detection: Detection::ContainerCreate,
                container: Some(exact("a".repeat(63))),
                reason: Some("CreateContainerConfigError"),
                exit_code: Some(i32::MIN),
                pods_seen: usize::MAX,
                pods_seen_is_minimum: true,
                pods_now: usize::MAX,
                revision: None,
            },
            namespace: numbered(63),
            owner: OwnerRef {
                api_version: exact(format!("{}/{}", "g".repeat(253), "v".repeat(63))),
                kind: exact("K".repeat(63)),
                name: numbered(253),
                uid: numbered(36),
            },
            pod: Some(numbered(253)),
            fingerprint: exact(format!("{i:016x}")),
            first_seen: Timestamp::UNIX_EPOCH,
            incident: Some(Box::new(IncidentData {
                settled_at: Timestamp::UNIX_EPOCH,
                owner_lookup_failed: true,
                about: About::Sample {
                    pod_uid: Uid::parse(UID),
                    controller: Some(Controller {
                        kind: ControllerKind::ReplicaSet,
                        uid: Uid::parse(UID),
                        replica_set: Some(numbered(253).into_boxed_str()),
                    }),
                },
            })),
        }
    }

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let mut queue = Queue::default();
        for i in 0..FACT_QUEUE_CAPACITY {
            assert_eq!(queue.push(fact(i)), None);
        }
        assert_eq!(queue.len(), FACT_QUEUE_CAPACITY);
        let dropped = queue.push(fact(FACT_QUEUE_CAPACITY)).expect("the oldest");
        assert_eq!(dropped.owner.name, "web-0000");
        assert_eq!(queue.len(), FACT_QUEUE_CAPACITY);
        assert_eq!(queue.pop().expect("an entry").owner.name, "web-0001");
    }

    #[test]
    fn a_dropped_fact_is_counted_and_owes_the_capped_message() {
        let metrics = Metrics::default();
        // A webhook that is never reached: the test only queues.
        let notifier = notify::Notifier::new(
            "http://192.0.2.1/hook".parse().expect("a URL"),
            notify::Format::Json,
            None,
            true,
        )
        .expect("a notifier");
        let term = Term::unbounded();
        let outbox = Outbox::new(&notifier, std::time::Duration::ZERO, 30, &metrics, &term);
        let mut queue = Queue::default();
        for i in 0..FACT_QUEUE_CAPACITY {
            enqueue(&mut queue, fact(i), &metrics, Some(&outbox)).expect("the log is written");
        }
        let now = tokio::time::Instant::now();
        assert_eq!(outbox.take_next(None, now).expect("the log"), None);
        for i in FACT_QUEUE_CAPACITY..FACT_QUEUE_CAPACITY + 3 {
            enqueue(&mut queue, fact(i), &metrics, Some(&outbox)).expect("the log is written");
        }
        assert_eq!(queue.len(), FACT_QUEUE_CAPACITY);
        assert!(metrics.render().contains("\nkausa_facts_dropped_total 3\n"));
        assert_eq!(
            outbox.take_next(None, now).expect("the log"),
            Some(notify::Outgoing::Capped)
        );
        // Without a webhook, a dropped fact is only counted.
        let mut queue = Queue::default();
        for i in 0..=FACT_QUEUE_CAPACITY {
            enqueue(&mut queue, fact(i), &metrics, None).expect("the log is written");
        }
        assert!(metrics.render().contains("\nkausa_facts_dropped_total 4\n"));
    }

    #[test]
    fn the_statement_of_an_entry_is_rendered_from_its_facts() {
        let entry = fact(1);
        assert_eq!(entry.statement(), fact::render(&entry.facts));
        assert_eq!(
            entry.statement(),
            "crash loop (D001). Sample: container app, reason CrashLoopBackOff, exit code 1. \
             Pods seen failing: 2. Pods now: 100."
        );
    }

    #[test]
    fn a_full_queue_of_the_longest_entries_is_within_the_computed_bound() {
        let mut queue = Queue::default();
        for i in 0..FACT_QUEUE_CAPACITY {
            queue.push(longest(i));
        }
        let entry = longest(0);
        assert!(entry.statement().len() <= LONGEST_STATEMENT_BYTES);
        assert!(entry.statement().len() < MAX_STATEMENT_BYTES);
        let off = |i| Fact {
            incident: None,
            ..longest(i)
        };
        let mut queue_off = Queue::default();
        for i in 0..FACT_QUEUE_CAPACITY {
            queue_off.push(off(i));
        }
        // The figures, pinned, on a 64-bit target: with incidents off, 1,344 bytes an entry, of
        // which 8 are the empty pointer, and 1,376,256 (1.31 MiB) a full queue; with incidents on,
        // 1,685 bytes an entry, of which the Incident data is 88 and its name 253, and 1,725,440
        // (1.65 MiB) a full queue. They come first, so that a failure of the bounds below, which
        // changes them, prints them; the bounds format nothing.
        assert_eq!((size_of::<Fact>(), size_of::<IncidentData>()), (280, 88));
        assert_eq!((off(0).bytes(), entry.bytes()), (1344, 1685));
        assert_eq!((queue_off.bytes(), queue.bytes()), (1_376_256, 1_725_440));
        // 0027 computes 1,583 bytes for an entry that holds the rendered statement, with incidents
        // off; an entry that holds the facts it is rendered from takes less, so a full queue stays
        // under the 1.55 MiB it computes. With incidents on, a full queue stays under the 2 MiB of
        // 0028's bar 2.
        assert!(off(0).bytes() <= 1583);
        assert!(queue_off.bytes() <= 1_620_992);
        assert!(queue.bytes() <= 2 * 1024 * 1024);
    }

    #[test]
    fn the_bytes_of_an_entry_count_the_name_of_its_regarding() {
        // A `D011` about a ReplicaSet with the longest name: its name takes the field the sample's
        // ReplicaSet takes, so the entry holds as much as the longest one about a Pod.
        let about = |name: Option<Box<str>>| Fact {
            facts: fact::Facts {
                detection: Detection::PodCreate,
                ..longest(0).facts
            },
            incident: Some(Box::new(IncidentData {
                settled_at: Timestamp::UNIX_EPOCH,
                owner_lookup_failed: true,
                about: About::Regarding {
                    kind: Some("ReplicaSet"),
                    name,
                    uid: MaybeUid::Uid(Uid::parse(UID).expect("a UID")),
                },
            })),
            ..longest(0)
        };
        let name = "r".repeat(253).into_boxed_str();
        assert_eq!(about(Some(name)).bytes() - about(None).bytes(), 253);
        assert_eq!(about(Some("r".repeat(253).into())).bytes(), 1685);
    }

    #[test]
    fn the_incident_data_keeps_checked_values_only() {
        let owner = |api_version: &str, kind: &str, name: &str, uid: &str| OwnerRef {
            api_version: api_version.into(),
            kind: kind.into(),
            name: name.into(),
            uid: uid.into(),
        };
        let uid = Uid::parse(UID);
        // An `apps/v1` ReplicaSet keeps its name for the read of its revision.
        assert_eq!(
            Controller::of(&owner("apps/v1", "ReplicaSet", "web-7d9c8b6f4d", UID)),
            Controller {
                kind: ControllerKind::ReplicaSet,
                uid,
                replica_set: Some("web-7d9c8b6f4d".into()),
            }
        );
        // A name the owner lookup refuses, a ReplicaSet of another group and any other kind keep
        // none, and a UID that is not one is kept as `None`.
        for (api_version, kind, name, kept_kind) in [
            (
                "apps/v1",
                "ReplicaSet",
                "Web/../x",
                ControllerKind::ReplicaSet,
            ),
            (
                "apps/v1",
                "ReplicaSet",
                &"a".repeat(254),
                ControllerKind::ReplicaSet,
            ),
            (
                "example.com/v1",
                "ReplicaSet",
                "web",
                ControllerKind::ReplicaSet,
            ),
            ("batch/v1", "Job", "job", ControllerKind::Job),
            ("apps/v1", "StatefulSet", "db", ControllerKind::StatefulSet),
            ("example.com/v1", "Rollout", "web", ControllerKind::Other),
        ] {
            let controller = Controller::of(&owner(api_version, kind, name, "not-a-uid"));
            assert_eq!(
                controller,
                Controller {
                    kind: kept_kind,
                    uid: None,
                    replica_set: None,
                },
                "{api_version} {kind}"
            );
        }

        let regarding = |kind: &str, name: &str, uid: Option<&str>| Regarding {
            api_version: "batch/v1".into(),
            kind: kind.into(),
            namespace: "shop".into(),
            name: name.into(),
            uid: uid.map(str::to_owned),
        };
        assert_eq!(
            About::regarding(&regarding("Job", "nightly", Some(UID))),
            About::Regarding {
                kind: Some("Job"),
                name: Some("nightly".into()),
                uid: MaybeUid::Uid(uid.expect("a UID")),
            }
        );
        assert_eq!(
            About::regarding(&regarding("Node", "Nightly", None)),
            About::Regarding {
                kind: None,
                name: None,
                uid: MaybeUid::Absent,
            }
        );
        assert_eq!(
            About::regarding(&regarding("Job", "nightly", Some(""))),
            About::Regarding {
                kind: Some("Job"),
                name: Some("nightly".into()),
                uid: MaybeUid::NotAUid,
            }
        );

        let pod = PodRecord {
            namespace: "shop".into(),
            name: "web-7d9c8b6f4d-abcde".into(),
            uid: "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0".into(),
            ..PodRecord::default()
        };
        assert_eq!(
            About::sample(&pod),
            About::Sample {
                pod_uid: None,
                controller: None,
            }
        );
        assert_eq!(
            About::deployment(None),
            About::Deployment {
                generation: None,
                revision: None,
            }
        );
    }
}
