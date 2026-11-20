//! The fact path (docs/adr/0027-fact-statement-for-the-detections.md, points 3 and 5): a settled
//! incident's statement goes on a bounded queue, and one slot of its own writes a `Detected`
//! Event for it, whatever the model, the budget and the breaker do.
//!
//! Prototype of a proposed ADR, on a branch that is not merged: it exists to measure the bounds of
//! the ADR's point 10. Left out: the `detected` webhook message and its limit per hour, the chart
//! and its RBAC, and the mark of point 4. With no webhook, a fingerprint is marked in the notified
//! table once its Event is written, so that the table holds what it would hold.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;

use k8s_openapi::jiff::Timestamp;
use serde_json::json;

use crate::events::{Emitted, Writer};
use crate::incident::Incident;
use crate::leader::Term;
use crate::log;
use crate::metrics::Metrics;
use crate::record::OwnerRef;

/// The most facts that wait for the fact slot. A fact pushed on a full queue drops the oldest.
pub const FACT_QUEUE_CAPACITY: usize = 1024;

/// The most fingerprints the notified table holds. One more drops the oldest.
pub const NOTIFIED_CAPACITY: usize = 1024;

/// What the fact slot needs of a settled incident: the statement and where its Event goes. Not
/// the incident itself, whose Pod set and sample message have no bound this small.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub statement: String,
    pub namespace: String,
    pub owner: OwnerRef,
    /// The sample's Pod, the Event's `related`.
    pub pod: Option<String>,
    pub fingerprint: String,
    pub first_seen: Timestamp,
}

impl Fact {
    pub fn new(incident: &Incident, statement: String) -> Self {
        Self {
            statement,
            namespace: incident.key.namespace.clone(),
            owner: incident.owner.clone(),
            pod: incident.sample.pod.clone(),
            fingerprint: incident.fingerprint.clone(),
            first_seen: incident.first_seen,
        }
    }

    /// The bytes this entry asks of the allocator, with the entry itself: what a full queue is
    /// computed from, beside what the process is measured to grow by.
    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.statement.capacity()
            + self.namespace.capacity()
            + self.owner.api_version.capacity()
            + self.owner.kind.capacity()
            + self.owner.name.capacity()
            + self.owner.uid.capacity()
            + self.pod.as_ref().map_or(0, String::capacity)
            + self.fingerprint.capacity()
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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes the queue holds: its entries' and the ring's own.
    pub fn bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Fact>()
            + self
                .entries
                .iter()
                .map(|fact| fact.bytes() - size_of::<Fact>())
                .sum::<usize>()
    }
}

/// Adds `fact` to `queue`; a fact dropped to make room is logged and counted.
pub fn enqueue(queue: &mut Queue, fact: Fact, metrics: &Metrics) -> io::Result<()> {
    if let Some(dropped) = queue.push(fact) {
        metrics.inc("kausa_facts_dropped_total", &[]);
        log::warn(
            "fact dropped",
            json!({"fingerprint": dropped.fingerprint, "reason": "queue full"}),
        )?;
    }
    Ok(())
}

/// The fingerprints already notified, oldest first, at most `NOTIFIED_CAPACITY`. It lives as long
/// as the process, across terms.
#[derive(Default)]
pub struct Notified {
    fingerprints: VecDeque<String>,
}

impl Notified {
    /// Marks `fingerprint`; one already there stays where it is.
    pub fn mark(&mut self, fingerprint: &str) {
        if self.holds(fingerprint) {
            return;
        }
        if self.fingerprints.len() >= NOTIFIED_CAPACITY {
            self.fingerprints.pop_front();
        }
        self.fingerprints.push_back(fingerprint.to_owned());
    }

    pub fn holds(&self, fingerprint: &str) -> bool {
        self.fingerprints.iter().any(|known| known == fingerprint)
    }

    pub fn len(&self) -> usize {
        self.fingerprints.len()
    }

    /// The bytes the table holds: its fingerprints' and the ring's own.
    pub fn bytes(&self) -> usize {
        self.fingerprints.capacity() * size_of::<String>()
            + self
                .fingerprints
                .iter()
                .map(String::capacity)
                .sum::<usize>()
    }
}

/// What the fact slot works with, across terms: the Event writer and the notified table.
pub struct Reporter<'a> {
    pub writer: Writer,
    pub notified: RefCell<Notified>,
    pub metrics: &'a Metrics,
    pub term: &'a Term,
}

impl Reporter<'_> {
    /// Writes the `Detected` Event of `fact`: one create, or the patch of its series. A failed
    /// write is logged and counted, and not tried again.
    pub async fn report(&self, fact: Fact) -> io::Result<()> {
        match self
            .writer
            .emit_detected(&fact, Timestamp::now(), self.term)
            .await
        {
            Ok(Emitted::Written) => {
                self.metrics
                    .inc("kausa_events_written_total", &[("reason", "Detected")]);
                self.notified.borrow_mut().mark(&fact.fingerprint);
                Ok(())
            }
            Ok(Emitted::Skipped) => Ok(()),
            Err(error) => {
                self.metrics.inc("kausa_event_write_errors_total", &[]);
                log::warn(
                    "event write failed",
                    json!({"fingerprint": fact.fingerprint, "error": error}),
                )
            }
        }
    }
}

/// The measurement harness of the prototype. Not a product flag and not for a release: it exists
/// only on this branch, to fill the queue and to write Events without a thousand real failures.
/// `KAUSA_PROTOTYPE_0027_HARNESS` selects it:
///
/// - `fill:<seconds>`: that long into a term, fills the fact queue and the notified table with
///   entries whose names are the longest Kubernetes allows, and holds them (the slot stays idle).
/// - `events:<seconds>:<namespace>`: that long into a term, queues `FACT_QUEUE_CAPACITY` facts
///   about Deployments that do not exist in `<namespace>`, which the slot writes one at a time.
pub mod harness {
    use std::time::Duration;

    use super::*;
    use crate::detect::Detection;
    use crate::fact;

    pub const VARIABLE: &str = "KAUSA_PROTOTYPE_0027_HARNESS";

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Harness {
        Fill { after: Duration },
        Events { after: Duration, namespace: String },
    }

    impl Harness {
        pub fn after(&self) -> Duration {
            match self {
                Self::Fill { after } | Self::Events { after, .. } => *after,
            }
        }
    }

    /// The harness the environment asks for, if any.
    pub fn from_environment() -> Option<Harness> {
        parse(&std::env::var(VARIABLE).ok()?)
    }

    pub fn parse(value: &str) -> Option<Harness> {
        let mut parts = value.splitn(3, ':');
        let kind = parts.next()?;
        let after = Duration::from_secs(parts.next()?.parse().ok()?);
        match (kind, parts.next()) {
            ("fill", None) => Some(Harness::Fill { after }),
            ("events", Some(namespace)) if !namespace.is_empty() => Some(Harness::Events {
                after,
                namespace: namespace.to_owned(),
            }),
            _ => None,
        }
    }

    /// The longest statement a real incident gives: the detection with the longest name, a
    /// container name of 63 bytes, its longest reason, the longest exit code and Pod counts of
    /// twenty digits.
    fn longest_statement() -> String {
        fact::render(&fact::Facts {
            detection: Detection::ContainerCreate,
            container: Some("a".repeat(63)),
            reason: Some("CreateContainerConfigError"),
            exit_code: Some(i32::MIN),
            pods_seen: usize::MAX,
            pods_seen_is_minimum: true,
            pods_now: usize::MAX,
            revision: None,
        })
    }

    /// The `i`th entry with every name at the longest Kubernetes allows: a namespace of 63 bytes,
    /// an owner of a group of 253 bytes and a version and a kind of 63, an owner and a Pod named
    /// with 253 bytes, and a UID of 36.
    pub fn longest(i: usize) -> Fact {
        // Each name holds exactly its bytes, as the clones `Fact::new` takes of a real incident's
        // names do; the statement keeps the capacity `fact::render` leaves it, as a real one does.
        let exact = |text: String| text.into_boxed_str().into_string();
        let numbered = |length: usize| exact(format!("{i:04}{}", "a".repeat(length - 4)));
        Fact {
            statement: longest_statement(),
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
        }
    }

    /// The `i`th fact of the Event run: a Deployment that does not exist in `namespace`.
    pub fn synthetic(i: usize, namespace: &str) -> Fact {
        Fact {
            statement: fact::render(&fact::Facts {
                detection: Detection::CrashLoop,
                container: Some("app".into()),
                reason: Some("CrashLoopBackOff"),
                exit_code: Some(1),
                pods_seen: 1 + i % 99,
                pods_seen_is_minimum: false,
                pods_now: 100,
                revision: None,
            }),
            namespace: namespace.to_owned(),
            owner: OwnerRef {
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                name: format!("harness-{i:04}"),
                uid: String::new(),
            },
            pod: Some(format!("harness-{i:04}-0")),
            fingerprint: format!("{i:016x}"),
            first_seen: Timestamp::now(),
        }
    }

    /// The resident memory of this process in kB (`VmRSS` of `/proc/self/status`); `None` where
    /// there is no such file.
    pub fn resident_kb() -> Option<u64> {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))?
            .trim()
            .strip_suffix("kB")?
            .trim()
            .parse()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::harness::Harness;
    use super::*;
    use crate::fact::{LONGEST_STATEMENT_BYTES, MAX_STATEMENT_BYTES};

    fn fact(i: usize) -> Fact {
        harness::synthetic(i, "shop")
    }

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let mut queue = Queue::default();
        for i in 0..FACT_QUEUE_CAPACITY {
            assert_eq!(queue.push(fact(i)), None);
        }
        assert_eq!(queue.len(), FACT_QUEUE_CAPACITY);
        let dropped = queue.push(fact(FACT_QUEUE_CAPACITY)).expect("the oldest");
        assert_eq!(dropped.owner.name, "harness-0000");
        assert_eq!(queue.len(), FACT_QUEUE_CAPACITY);
        assert_eq!(queue.pop().expect("an entry").owner.name, "harness-0001");
    }

    #[test]
    fn a_dropped_fact_is_counted() {
        let metrics = Metrics::default();
        let mut queue = Queue::default();
        for i in 0..FACT_QUEUE_CAPACITY + 3 {
            enqueue(&mut queue, fact(i), &metrics).expect("the log is written");
        }
        assert_eq!(queue.len(), FACT_QUEUE_CAPACITY);
        assert!(metrics.render().contains("\nkausa_facts_dropped_total 3\n"));
    }

    #[test]
    fn the_notified_table_keeps_the_newest() {
        let mut notified = Notified::default();
        for i in 0..NOTIFIED_CAPACITY {
            notified.mark(&format!("{i:016x}"));
            notified.mark(&format!("{i:016x}"));
        }
        assert_eq!(notified.len(), NOTIFIED_CAPACITY);
        assert!(notified.holds(&format!("{:016x}", 0)));
        notified.mark(&format!("{NOTIFIED_CAPACITY:016x}"));
        assert_eq!(notified.len(), NOTIFIED_CAPACITY);
        assert!(!notified.holds(&format!("{:016x}", 0)));
        assert!(notified.holds(&format!("{:016x}", 1)));
        assert!(notified.holds(&format!("{NOTIFIED_CAPACITY:016x}")));
        // Under 64 bytes for each fingerprint, ring and all.
        assert!(
            notified.bytes() < NOTIFIED_CAPACITY * 64,
            "{}",
            notified.bytes()
        );
    }

    #[test]
    fn a_full_queue_of_the_longest_entries_is_under_two_mebibytes() {
        let mut queue = Queue::default();
        for i in 0..FACT_QUEUE_CAPACITY {
            queue.push(harness::longest(i));
        }
        let entry = harness::longest(0);
        assert!(entry.statement.len() <= LONGEST_STATEMENT_BYTES);
        assert!(entry.statement.len() < MAX_STATEMENT_BYTES);
        assert!(queue.bytes() <= 2 * 1024 * 1024, "{}", queue.bytes());
        // The figures the research note quotes, pinned.
        assert_eq!(
            (entry.statement.len(), entry.statement.capacity()),
            (269, 374)
        );
        assert_eq!((size_of::<Fact>(), entry.bytes()), (208, 1583));
        assert_eq!(queue.bytes(), 1_620_992);
    }

    #[test]
    fn the_harness_is_read_from_its_variable() {
        assert_eq!(
            harness::parse("fill:60"),
            Some(Harness::Fill {
                after: Duration::from_secs(60)
            })
        );
        assert_eq!(
            harness::parse("events:45:harness"),
            Some(Harness::Events {
                after: Duration::from_secs(45),
                namespace: "harness".into()
            })
        );
        for value in [
            "",
            "fill",
            "fill:x",
            "fill:1:x",
            "events:1",
            "events:1:",
            "other:1",
        ] {
            assert_eq!(harness::parse(value), None, "{value:?}");
        }
    }
}
