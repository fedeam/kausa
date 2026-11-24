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
use crate::record::OwnerRef;

/// The most facts that wait for the fact slot. A fact pushed on a full queue drops the oldest.
pub const FACT_QUEUE_CAPACITY: usize = 1024;

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
        }
    }

    /// The `i`th entry with every name at the longest Kubernetes allows: a container name of 63
    /// bytes, a namespace of 63, an owner of a group of 253 bytes and a version and a kind of 63,
    /// an owner and a Pod named with 253 bytes, and a UID of 36; and the longest statement: the
    /// detection with the longest name, its longest reason, the longest exit code and Pod counts
    /// of twenty digits.
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
        // The ADR computes 1,583 bytes for an entry that holds the rendered statement; an entry
        // that holds the facts it is rendered from takes less, so a full queue stays under the
        // 1.55 MiB it computes.
        assert!(entry.bytes() <= 1583, "{}", entry.bytes());
        assert!(queue.bytes() <= 1_620_992, "{}", queue.bytes());
        // The figures, pinned: 1,336 bytes an entry and 1,368,064 (1.30 MiB) a full queue, on a
        // 64-bit target.
        assert_eq!((size_of::<Fact>(), entry.bytes()), (272, 1336));
        assert_eq!(queue.bytes(), 1_368_064);
    }
}
