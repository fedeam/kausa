//! The table of created Incidents and the gate in front of each create
//! (docs/adr/0028-incident-resource-and-remediation-report.md, points 1, 4 and 6).
//!
//! Pure: no I/O and no clock of its own; every instant comes in from the caller. The table holds
//! at most `CAPACITY` entries, one per Incident this process created, each in a slot of its own
//! from its create to its drop, so that a drop moves no other entry. An entry is made only from
//! the UID the server gave the create, so a create the server refused, `409 AlreadyExists`
//! included, never marks a fingerprint. An entry younger than `KEEP` is never dropped; an older
//! one is dropped, oldest first, only to make room.

use std::time::{Duration, Instant};

use k8s_openapi::jiff::Timestamp;

use super::{Spec, Uid, is_fingerprint};
use crate::detect::Detection;
use crate::owner::is_dns_label;

/// The most entries the table holds.
pub(crate) const CAPACITY: usize = 500;

/// How long an entry is kept for certain: kurita's window to decide an Incident.
pub(crate) const KEEP: Duration = Duration::from_secs(6 * 3600);

/// How often `incident refused` is logged at most, for each reason.
pub(crate) const REFUSED_LOG_PERIOD: Duration = Duration::from_secs(3600);

/// Why kausa created no Incident for an incident: the label `reason` of
/// `kausa_incidents_refused_total`, and the reason of the `incident refused` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Refusal {
    /// The table holds `CAPACITY` entries younger than `KEEP`.
    TableFull,
    /// A value does not fit its field of the schema (`Spec::check`).
    Schema,
    /// No sweep of this process has ended yet: the create does not wait for one.
    Starting,
}

impl Refusal {
    pub(crate) const ALL: [Refusal; 3] = [Refusal::TableFull, Refusal::Schema, Refusal::Starting];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Refusal::TableFull => "table-full",
            Refusal::Schema => "schema",
            Refusal::Starting => "starting",
        }
    }
}

/// The gate's answer for one incident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Send the create.
    Create,
    /// An Incident of this namespace and fingerprint was created inside the window: no create, and
    /// no refusal.
    Duplicate,
    /// No create, counted and logged with its reason.
    Refused(Refusal),
}

/// The object an Incident is about, as kausa sent it: what a Remediation's `target` must equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    api_version: Box<str>,
    kind: Box<str>,
    name: Box<str>,
    uid: Option<Uid>,
}

impl Target {
    #[cfg(test)]
    pub(crate) fn api_version(&self) -> &str {
        &self.api_version
    }

    #[cfg(test)]
    pub(crate) fn kind(&self) -> &str {
        &self.kind
    }

    #[cfg(test)]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    #[cfg(test)]
    pub(crate) fn uid(&self) -> Option<Uid> {
        self.uid
    }
}

/// What kausa keeps of one Incident it created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    namespace: Box<str>,
    fingerprint: u64,
    first_seen: Timestamp,
    uid: Uid,
    detection: Detection,
    target: Target,
    /// When the server's answer accepting the create arrived, on the process's monotonic clock.
    created: Instant,
    /// Whether a `remediation` message was queued for it.
    reported: bool,
}

/// The value of `text` if it is a fingerprint as `is_fingerprint` reads one, 16 lower-case
/// hexadecimal digits; `u64::from_str_radix` alone also takes a sign, upper case and fewer digits.
fn parse_fingerprint(text: &str) -> Option<u64> {
    is_fingerprint(text)
        .then(|| u64::from_str_radix(text, 16).ok())
        .flatten()
}

impl Entry {
    /// The entry of the Incident of `spec` that the server created in `namespace` with `uid`, its
    /// answer arriving at `created`. `None` for a spec the check refuses or a namespace that is
    /// not a DNS label, for neither of which the server accepts a create.
    pub(crate) fn new(namespace: &str, spec: &Spec, uid: Uid, created: Instant) -> Option<Entry> {
        if !is_dns_label(namespace) || spec.check().is_err() {
            return None;
        }
        let object = &spec.object;
        let target_uid = match &object.uid {
            Some(text) => Some(Uid::parse(text)?),
            None => None,
        };
        Some(Entry {
            namespace: namespace.into(),
            fingerprint: parse_fingerprint(&spec.fingerprint)?,
            first_seen: spec.first_seen,
            uid,
            detection: spec.detection,
            target: Target {
                api_version: object.api_version.as_str().into(),
                kind: object.kind.as_str().into(),
                name: object.name.as_str().into(),
                uid: target_uid,
            },
            created,
            reported: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The fingerprint, as its 16 hexadecimal characters.
    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> String {
        format!("{:016x}", self.fingerprint)
    }

    #[cfg(test)]
    pub(crate) fn first_seen(&self) -> Timestamp {
        self.first_seen
    }

    #[cfg(test)]
    pub(crate) fn uid(&self) -> Uid {
        self.uid
    }

    #[cfg(test)]
    pub(crate) fn detection(&self) -> Detection {
        self.detection
    }

    #[cfg(test)]
    pub(crate) fn target(&self) -> &Target {
        &self.target
    }

    #[cfg(test)]
    pub(crate) fn reported(&self) -> bool {
        self.reported
    }

    #[cfg(test)]
    pub(crate) fn created(&self) -> Instant {
        self.created
    }

    fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.created)
    }

    /// Whether the Incident's name is `name`: `kausa.`, the fingerprint, `.` and the first-seen
    /// second, written without a leading zero.
    #[cfg(test)]
    fn is_named(&self, name: &str) -> bool {
        let Some((fingerprint, second)) = name
            .strip_prefix("kausa.")
            .and_then(|rest| rest.split_once('.'))
        else {
            return false;
        };
        parse_fingerprint(fingerprint) == Some(self.fingerprint)
            && (1..=10).contains(&second.len())
            && second.bytes().all(|b| b.is_ascii_digit())
            && (second == "0" || !second.starts_with('0'))
            && second.parse::<i64>() == Ok(self.first_seen.as_second())
    }

    /// The bytes this entry asks of the allocator beyond its slot.
    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        self.namespace.len()
            + self.target.api_version.len()
            + self.target.kind.len()
            + self.target.name.len()
    }
}

/// Where an entry sits in the table, from a lookup. It names that entry only while the slot holds
/// it: `Table::mark_reported` checks the UID again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct Slot(usize);

/// The table was full: `CAPACITY` entries younger than `KEEP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Full;

/// The Incidents this process created, at most `CAPACITY`, each in a fixed slot.
#[derive(Debug)]
pub(crate) struct Table {
    slots: Vec<Option<Entry>>,
    started: bool,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            slots: vec![None; CAPACITY],
            started: false,
        }
    }
}

impl Table {
    /// Records that a sweep of this process ended, whether it completed, failed or stopped at its
    /// bound: from then on the gate lets creates through. A sweep cut by the loss of the term has
    /// not ended.
    pub(crate) fn start_sweep_ended(&mut self) {
        self.started = true;
    }

    /// Whether a sweep of this process ended: the gate's flag.
    #[cfg(test)]
    pub(crate) fn sweep_ended(&self) -> bool {
        self.started
    }

    /// The gate for an incident of `namespace` and `fingerprint` at `now`, with `--dedup-hours` as
    /// `window`, in this order: a duplicate when an entry of both was created inside the window
    /// (none with a window of zero); refused before a sweep of this process ended; refused when no
    /// slot is free and every entry is younger than `KEEP`; a create otherwise. It changes nothing.
    pub(crate) fn admit(
        &self,
        namespace: &str,
        fingerprint: &str,
        window: Duration,
        now: Instant,
    ) -> Admission {
        let fingerprint = parse_fingerprint(fingerprint);
        let duplicate = self.entries().any(|(_, entry)| {
            Some(entry.fingerprint) == fingerprint
                && *entry.namespace == *namespace
                && entry.age(now) < window
        });
        if duplicate {
            Admission::Duplicate
        } else if !self.started {
            Admission::Refused(Refusal::Starting)
        } else if self.room(now).is_none() {
            Admission::Refused(Refusal::TableFull)
        } else {
            Admission::Create
        }
    }

    /// Keeps `entry`, in a free slot or, with none free, in the slot of the oldest entry older than
    /// `KEEP`, which is dropped and returned. `Full` when every entry is younger than `KEEP`.
    pub(crate) fn insert(&mut self, entry: Entry, now: Instant) -> Result<Option<Entry>, Full> {
        let slot = self.room(now).ok_or(Full)?;
        Ok(self.slots[slot].replace(entry))
    }

    /// The Incident named `name` in `namespace` whose UID is `uid`, if this process created it and
    /// still holds it.
    #[cfg(test)]
    pub(crate) fn find(&self, namespace: &str, name: &str, uid: Uid) -> Option<(Slot, &Entry)> {
        self.entries()
            .find(|(_, entry)| {
                entry.uid == uid && *entry.namespace == *namespace && entry.is_named(name)
            })
            .map(|(slot, entry)| (Slot(slot), entry))
    }

    /// Marks the entry at `slot` as reported, if the slot still holds the Incident of `uid` and it
    /// was not reported before; whether it marked it.
    #[cfg(test)]
    pub(crate) fn mark_reported(&mut self, slot: Slot, uid: Uid) -> bool {
        match self.slots.get_mut(slot.0).and_then(Option::as_mut) {
            Some(entry) if entry.uid == uid && !entry.reported => {
                entry.reported = true;
                true
            }
            _ => false,
        }
    }

    /// How many entries the table holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries().count()
    }

    fn entries(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| entry.as_ref().map(|entry| (slot, entry)))
    }

    /// The slot a new entry takes at `now`: the first free one, else the oldest entry's that is at
    /// least `KEEP` old.
    fn room(&self, now: Instant) -> Option<usize> {
        if let Some(free) = self.slots.iter().position(Option::is_none) {
            return Some(free);
        }
        self.entries()
            .filter(|(_, entry)| entry.age(now) >= KEEP)
            .max_by_key(|(_, entry)| entry.age(now))
            .map(|(slot, _)| slot)
    }

    /// The bytes the table holds: its slots and its entries' names.
    #[cfg(test)]
    fn bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Option<Entry>>()
            + self.entries().map(|(_, e)| e.heap_bytes()).sum::<usize>()
    }
}

/// At most one `incident refused` line per `REFUSED_LOG_PERIOD` for each reason; the occurrences in
/// between are counted, not logged.
#[derive(Debug, Default)]
pub(crate) struct Limiter {
    last: [Option<Instant>; Refusal::ALL.len()],
}

impl Limiter {
    /// Whether a refusal for `reason` at `now` is logged; if so, the period starts again at `now`.
    pub(crate) fn allow(&mut self, reason: Refusal, now: Instant) -> bool {
        let last = &mut self.last[reason as usize];
        let allowed = last.is_none_or(|at| now.saturating_duration_since(at) >= REFUSED_LOG_PERIOD);
        if allowed {
            *last = Some(now);
        }
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident_resource::tests::pod_spec;
    use crate::owner::{MAX_NAME_LEN, MAX_NAMESPACE_LEN};

    const HOUR: Duration = Duration::from_secs(3600);

    fn uid(i: usize) -> Uid {
        Uid::parse(&format!("00000000-0000-4000-8000-{i:012x}")).expect("a UID")
    }

    fn fingerprint(i: usize) -> String {
        format!("{i:016x}")
    }

    /// The Incident of the `i`th fingerprint, first seen at second `1_795_514_400 + i`.
    fn spec(i: usize) -> Spec {
        Spec {
            fingerprint: fingerprint(i),
            first_seen: Timestamp::from_second(1_795_514_400 + i as i64).expect("a time"),
            ..pod_spec()
        }
    }

    fn entry(i: usize, created: Instant) -> Entry {
        Entry::new("shop", &spec(i), uid(i), created).expect("an entry")
    }

    fn started() -> Table {
        let mut table = Table::default();
        table.start_sweep_ended();
        table
    }

    /// A table whose `i`th slot holds the `i`th entry, created at `created(i)`.
    fn filled(created: impl Fn(usize) -> Instant) -> Table {
        let mut table = started();
        for i in 0..CAPACITY {
            assert_eq!(table.insert(entry(i, created(i)), created(i)), Ok(None));
        }
        table
    }

    fn uids(table: &Table) -> Vec<Option<Uid>> {
        table
            .slots
            .iter()
            .map(|slot| slot.as_ref().map(Entry::uid))
            .collect()
    }

    #[test]
    fn the_refusal_reasons_are_a_closed_set_of_three() {
        let labels: Vec<&str> = Refusal::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(labels, ["table-full", "schema", "starting"]);
        for (i, reason) in Refusal::ALL.into_iter().enumerate() {
            assert_eq!(reason as usize, i, "{reason:?}");
        }
    }

    #[test]
    fn no_create_passes_before_the_sweep_at_start_ended() {
        let now = Instant::now();
        let mut table = Table::default();
        assert_eq!(
            table.admit("shop", &fingerprint(1), 6 * HOUR, now),
            Admission::Refused(Refusal::Starting)
        );
        // The refusal does not wait and marks nothing.
        assert_eq!(table.len(), 0);
        table.start_sweep_ended();
        assert_eq!(
            table.admit("shop", &fingerprint(1), 6 * HOUR, now),
            Admission::Create
        );
    }

    #[test]
    fn a_fingerprint_creates_once_per_window_in_its_namespace() {
        let t0 = Instant::now();
        let mut table = started();
        table.insert(entry(1, t0), t0).expect("room");
        let window = 6 * HOUR;
        let fp = fingerprint(1);
        assert_eq!(table.admit("shop", &fp, window, t0), Admission::Duplicate);
        assert_eq!(
            table.admit("shop", &fp, window, t0 + window - Duration::from_nanos(1)),
            Admission::Duplicate
        );
        assert_eq!(
            table.admit("shop", &fp, window, t0 + window),
            Admission::Create
        );
        // Another namespace, another fingerprint, or a window of zero: a create.
        assert_eq!(table.admit("cart", &fp, window, t0), Admission::Create);
        assert_eq!(
            table.admit("shop", &fingerprint(2), window, t0),
            Admission::Create
        );
        assert_eq!(
            table.admit("shop", &fp, Duration::ZERO, t0),
            Admission::Create
        );
        // Only the fingerprint's own spelling is a duplicate: upper case, a sign or fewer digits
        // name no entry.
        let fp = fingerprint(0xab);
        table.insert(entry(0xab, t0), t0).expect("room");
        for other in ["00000000000000AB", "+0000000000000ab", "ab"] {
            assert_eq!(u64::from_str_radix(other, 16), u64::from_str_radix(&fp, 16));
            assert_eq!(
                table.admit("shop", other, window, t0),
                Admission::Create,
                "{other}"
            );
        }
        assert_eq!(table.admit("shop", &fp, window, t0), Admission::Duplicate);
        let fp = fingerprint(1);
        // A window past 6 hours holds while the entry is kept.
        assert_eq!(
            table.admit("shop", &fp, 24 * HOUR, t0 + 12 * HOUR),
            Admission::Duplicate
        );
    }

    #[test]
    fn a_table_of_young_entries_refuses_and_drops_none() {
        let t0 = Instant::now();
        let mut table = filled(|_| t0);
        assert_eq!(table.len(), CAPACITY);
        let before = uids(&table);
        let young = t0 + KEEP - Duration::from_nanos(1);
        assert_eq!(
            table.admit("shop", &fingerprint(CAPACITY), 6 * HOUR, young),
            Admission::Refused(Refusal::TableFull)
        );
        assert_eq!(table.insert(entry(CAPACITY, young), young), Err(Full));
        assert_eq!(uids(&table), before);
        // A duplicate is still a duplicate on a full table.
        assert_eq!(
            table.admit("shop", &fingerprint(3), 6 * HOUR, young),
            Admission::Duplicate
        );
    }

    #[test]
    fn room_is_made_by_dropping_the_oldest_entry_past_six_hours_in_its_slot() {
        let t0 = Instant::now();
        // The entry of slot 7 is the oldest; slot 3's is next.
        let created = |i: usize| match i {
            7 => t0,
            3 => t0 + Duration::from_secs(1),
            _ => t0 + HOUR,
        };
        let mut table = filled(created);
        let before = uids(&table);
        let now = t0 + KEEP + Duration::from_secs(1);
        assert_eq!(
            table.admit("shop", &fingerprint(CAPACITY), 6 * HOUR, now),
            Admission::Create
        );
        let dropped = table
            .insert(entry(CAPACITY, now), now)
            .expect("room")
            .expect("a dropped entry");
        assert_eq!(dropped.uid(), uid(7));
        let mut after = before.clone();
        after[7] = Some(uid(CAPACITY));
        assert_eq!(uids(&table), after, "no other entry moved");
        // Next, slot 3's; then none is old enough.
        let dropped = table.insert(entry(CAPACITY + 1, now), now).expect("room");
        assert_eq!(dropped.map(|e| e.uid()), Some(uid(3)));
        assert_eq!(table.insert(entry(CAPACITY + 2, now), now), Err(Full));
        assert_eq!(table.len(), CAPACITY);
    }

    #[test]
    fn a_free_slot_is_taken_before_an_old_entry_is_dropped() {
        let t0 = Instant::now();
        let mut table = started();
        table.insert(entry(1, t0), t0).expect("room");
        let late = t0 + 2 * KEEP;
        assert_eq!(table.insert(entry(2, late), late), Ok(None));
        assert_eq!(table.len(), 2);
        assert_eq!(uids(&table)[..2], [Some(uid(1)), Some(uid(2))]);
    }

    #[test]
    fn a_lookup_needs_the_namespace_the_name_and_the_uid() {
        let t0 = Instant::now();
        let mut table = started();
        table.insert(entry(1, t0), t0).expect("room");
        table.insert(entry(2, t0), t0).expect("room");
        let name = spec(2).name();
        assert_eq!(name, "kausa.0000000000000002.1795514402");
        let (slot, found) = table.find("shop", &name, uid(2)).expect("the entry");
        assert_eq!(slot, Slot(1));
        assert_eq!(found.uid(), uid(2));
        assert_eq!(found.namespace(), "shop");
        assert_eq!(found.fingerprint(), fingerprint(2));
        assert_eq!(found.first_seen(), spec(2).first_seen);
        assert_eq!(found.detection(), Detection::CrashLoop);
        assert_eq!(table.find("cart", &name, uid(2)), None);
        assert_eq!(table.find("shop", &name, uid(1)), None);
        for other in [
            "kausa.0000000000000002.01795514402",
            "kausa.0000000000000002.1795514403",
            "kausa.0000000000000001.1795514402",
            "kausa.0000000000000002.1795514402.",
            "kurita.0000000000000002.1795514402",
            "kausa.000000000000002.1795514402",
            "kausa.0000000000000002.+1795514402",
            "",
        ] {
            assert_eq!(table.find("shop", other, uid(2)), None, "{other}");
        }
        // The second 0 has no leading zero to refuse.
        let epoch = Spec {
            fingerprint: fingerprint(9),
            first_seen: Timestamp::UNIX_EPOCH,
            settled_at: Timestamp::UNIX_EPOCH,
            ..pod_spec()
        };
        let entry = Entry::new("shop", &epoch, uid(9), t0).expect("an entry");
        table.insert(entry, t0).expect("room");
        assert!(
            table
                .find("shop", "kausa.0000000000000009.0", uid(9))
                .is_some()
        );
        assert!(
            table
                .find("shop", "kausa.0000000000000009.00", uid(9))
                .is_none()
        );
    }

    #[test]
    fn an_incident_is_reported_once_and_a_stale_slot_marks_nothing() {
        let t0 = Instant::now();
        let mut table = filled(|_| t0);
        let (slot, entry_1) = table
            .find("shop", &spec(1).name(), uid(1))
            .expect("the entry");
        assert!(!entry_1.reported());
        assert!(table.mark_reported(slot, uid(1)));
        assert!(!table.mark_reported(slot, uid(1)), "marked once");
        assert!(
            table
                .find("shop", &spec(1).name(), uid(1))
                .expect("kept")
                .1
                .reported()
        );
        // Once its entry is dropped, the slot holds another Incident, which it does not mark.
        let (slot, _) = table
            .find("shop", &spec(2).name(), uid(2))
            .expect("the entry");
        let later = t0 + KEEP + Duration::from_secs(10);
        for i in CAPACITY..2 * CAPACITY {
            table.insert(entry(i, later), later).expect("room");
        }
        assert_eq!(table.find("shop", &spec(2).name(), uid(2)), None);
        assert!(!table.mark_reported(slot, uid(2)));
        assert!(!table.slots[slot.0].as_ref().expect("an entry").reported());
        assert!(!table.mark_reported(Slot(CAPACITY), uid(2)), "no such slot");
    }

    #[test]
    fn an_entry_holds_what_the_server_accepted() {
        let t0 = Instant::now();
        let e = entry(5, t0);
        let object = pod_spec().object;
        assert_eq!(
            (
                e.target().api_version(),
                e.target().kind(),
                e.target().name()
            ),
            (
                object.api_version.as_str(),
                object.kind.as_str(),
                object.name.as_str()
            )
        );
        assert_eq!(e.target().uid(), object.uid.as_deref().and_then(Uid::parse));
        let mut no_uid = spec(5);
        no_uid.object.uid = None;
        let e = Entry::new("shop", &no_uid, uid(5), t0).expect("an entry");
        assert_eq!(e.target().uid(), None);
        // No entry for a spec the check refuses, or a namespace that is not a DNS label. A kind
        // in lower case and too many Pods are refused by the check alone; a fingerprint that is
        // not one fails its parse too.
        let mut kind = spec(5);
        kind.object.kind = "pod".into();
        let mut pods = spec(5);
        pods.facts.pods_now = super::super::MAX_PODS_NOW + 1;
        let mut fingerprint = spec(5);
        fingerprint.fingerprint = "X".repeat(16);
        for refused in [kind, pods, fingerprint] {
            assert!(refused.check().is_err());
            assert_eq!(Entry::new("shop", &refused, uid(5), t0), None);
        }
        for namespace in ["", "Shop", "a.b", &"a".repeat(MAX_NAMESPACE_LEN + 1)] {
            assert_eq!(
                Entry::new(namespace, &spec(5), uid(5), t0),
                None,
                "{namespace}"
            );
        }
    }

    #[test]
    fn a_refusal_is_logged_at_most_once_an_hour_for_each_reason() {
        let t0 = Instant::now();
        let mut limiter = Limiter::default();
        for reason in Refusal::ALL {
            assert!(limiter.allow(reason, t0), "{reason:?}");
            assert!(!limiter.allow(reason, t0), "{reason:?}");
        }
        let before = t0 + REFUSED_LOG_PERIOD - Duration::from_nanos(1);
        assert!(!limiter.allow(Refusal::Schema, before));
        assert!(limiter.allow(Refusal::Schema, t0 + REFUSED_LOG_PERIOD));
        // Each reason has its own period: schema's line did not take table-full's.
        assert!(!limiter.allow(Refusal::Schema, t0 + REFUSED_LOG_PERIOD));
        assert!(limiter.allow(Refusal::TableFull, t0 + REFUSED_LOG_PERIOD));
        assert!(!limiter.allow(Refusal::Starting, before));
        assert_eq!(REFUSED_LOG_PERIOD, HOUR);
        assert_eq!((CAPACITY, KEEP), (500, 6 * HOUR));
    }

    /// The `i`th entry with every name at its longest: a namespace of 63 bytes, an apiVersion of
    /// 253, a kind of 63 and a name of 253.
    fn longest(i: usize, created: Instant) -> Entry {
        let mut spec = spec(i);
        spec.object.api_version = format!("{}/{}", "g".repeat(189), "v".repeat(63));
        spec.object.kind = "K".repeat(63);
        spec.object.name = format!("{i:04}{}", "n".repeat(MAX_NAME_LEN - 4));
        let namespace = format!("{i:04}{}", "s".repeat(MAX_NAMESPACE_LEN - 4));
        Entry::new(&namespace, &spec, uid(i), created).expect("an entry")
    }

    #[test]
    fn a_full_table_of_the_longest_entries_is_within_bar_3() {
        let t0 = Instant::now();
        let mut table = started();
        // An empty table holds its slots already.
        let empty = table.bytes();
        for i in 0..CAPACITY {
            table.insert(longest(i, t0), t0).expect("room");
        }
        let entry = longest(0, t0);
        // The figures, pinned on a 64-bit target: they come first, so that a failure of the bound
        // below prints them.
        assert_eq!(
            (
                size_of::<Option<Entry>>(),
                entry.heap_bytes(),
                empty,
                table.bytes()
            ),
            (152, 632, 76_000, 392_000)
        );
        // 0028's bar 3: at most 0.5 MiB at 500 entries.
        assert!(table.bytes() <= 524_288);
    }
}
