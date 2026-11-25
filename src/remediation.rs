//! The parse and the checks of a kurita Remediation
//! (docs/adr/0028-incident-resource-and-remediation-report.md, point 6).
//!
//! Pure: no I/O, no clock and no Kubernetes type. The object comes in as JSON, the instants from
//! the caller. A Remediation is written by kurita, or by anyone who may create one in its
//! namespace, so it is untrusted: the parse reads only `metadata.namespace`, `metadata.name`,
//! `spec.incident.{name,uid,detection,fingerprint}`, `spec.target`, `spec.action`, `spec.mode` and
//! `spec.proposedAt`, each against the pattern and the length of kurita's schema
//! (`charts/kurita/files/remediation-schema.json` at kurita's commit
//! `64d8a358d765157dc4b68c08d8f33f9a0fa31646`), and keeps nothing else in any type: not its
//! `reason`, its parameters, its `preconditions`, its `decider` or any other field. A reportable
//! record gives only its action, its `proposedAt` and the table's entry it is about: the message
//! takes the detection, the object and the fingerprint from that entry ([`Report::notice`]),
//! never from the record.
//!
//! The checks, in this order; a record gets the reason of the first one that fails, and only
//! that one:
//!
//! 1. the parse: every field read has its shape, `metadata.name` is `kurita.<rest>` and
//!    `incident.name` is `kausa.<rest>`, `mode` is `proposed` (`invalid`);
//! 2. the table holds the Incident `kausa.<rest>` in the record's namespace, with the UID
//!    `incident.uid` (`incident-unknown`);
//! 3. `incident.detection` and `incident.fingerprint` equal the entry's (`invalid`);
//! 4. the action is one kausa knows (`unknown-action`);
//! 5. [`ALLOWED`] allows it for the entry's detection (`invalid`);
//! 6. `target` equals the entry's object (`target-mismatch`);
//! 7. `proposedAt` is within `[firstSeen, now]`, both ends included, `firstSeen` to the second
//!    as the Incident writes it (`time-out-of-range`);
//! 8. no message was queued for the Incident before (`duplicate`).
//!
//! A record that passes all eight with the action `none` is [`Outcome::Nothing`]: it is not sent,
//! not counted, and its Incident is not marked. The check never changes the table: the caller
//! marks the entry with `Table::mark_reported` once the message is queued.

use std::time::{Duration, Instant};

use k8s_openapi::jiff::Timestamp;
use serde_json::{Map, Value};

use crate::detect::Detection;
use crate::incident_resource::table::{Entry, Slot, Table, Target as Held};
use crate::incident_resource::{
    DETECTIONS, Uid, is_api_version, is_fingerprint, is_kind, is_time, time,
};
use crate::notification;
use crate::owner::{is_dns_label, is_dns_subdomain};

/// The start of a Remediation's name: kurita names it after its Incident, with `kurita.` in place
/// of `kausa.` (kurita's ADR 0008, point 2).
pub(crate) const NAME_PREFIX: &str = "kurita.";
/// The start of `incident.name`, kausa's own Incident name (kurita's schema, `:100`).
pub(crate) const INCIDENT_PREFIX: &str = "kausa.";
/// The most digits of the first-seen second in both names (`[0-9]{1,10}`, kurita's schema
/// `:100`).
pub(crate) const MAX_SECOND_DIGITS: usize = 10;
/// The longest `action` kurita's schema takes (`:185`): an id kausa does not know is read up to
/// this length, so that it is counted as `unknown-action` and not as `invalid`.
pub(crate) const ACTION_LEN: usize = 21;
/// The only `mode` (kurita's schema, `:190-193`).
pub(crate) const MODE: &str = "proposed";
/// The least time between two `remediation ignored` lines of one reason.
pub(crate) const IGNORED_LOG_PERIOD: Duration = Duration::from_secs(60);

/// Why a Remediation is not reported: the label of `kausa_remediations_ignored_total{reason}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    IncidentUnknown,
    Invalid,
    UnknownAction,
    TargetMismatch,
    TimeOutOfRange,
    Duplicate,
}

impl Reason {
    pub(crate) const ALL: [Reason; 6] = [
        Reason::IncidentUnknown,
        Reason::Invalid,
        Reason::UnknownAction,
        Reason::TargetMismatch,
        Reason::TimeOutOfRange,
        Reason::Duplicate,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Reason::IncidentUnknown => "incident-unknown",
            Reason::Invalid => "invalid",
            Reason::UnknownAction => "unknown-action",
            Reason::TargetMismatch => "target-mismatch",
            Reason::TimeOutOfRange => "time-out-of-range",
            Reason::Duplicate => "duplicate",
        }
    }
}

/// The action ids kausa knows: kurita's vocabulary (its schema, `:176-184`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    NeedsHuman,
    Inspect,
    RolloutUndo,
    DeleteEvictedPods,
    RaiseMemoryLimit,
    SetImagePullPolicy,
}

impl Action {
    pub(crate) const ALL: [Action; 7] = [
        Action::None,
        Action::NeedsHuman,
        Action::Inspect,
        Action::RolloutUndo,
        Action::DeleteEvictedPods,
        Action::RaiseMemoryLimit,
        Action::SetImagePullPolicy,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Action::None => "none",
            Action::NeedsHuman => "needs-human",
            Action::Inspect => "inspect",
            Action::RolloutUndo => "rollout-undo",
            Action::DeleteEvictedPods => "delete-evicted-pods",
            Action::RaiseMemoryLimit => "raise-memory-limit",
            Action::SetImagePullPolicy => "set-image-pull-policy",
        }
    }

    fn of(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|action| action.as_str() == id)
    }
}

/// Every detection but `D009` and `D014`: those `inspect` is allowed for.
const INSPECTED: [Detection; 12] = [
    Detection::CrashLoop,
    Detection::ImagePull,
    Detection::OutOfMemory,
    Detection::ContainerCreate,
    Detection::Unschedulable,
    Detection::ProbeFailure,
    Detection::VolumeFailure,
    Detection::JobFailed,
    Detection::SandboxCreate,
    Detection::PodCreate,
    Detection::ClaimProvisioning,
    Detection::AutoscalerMetrics,
];

/// kausa's copy of the actions each detection allows (0028, point 6, and kurita's ADR 0008,
/// point 2): `none` and `needs-human` for every detection; `inspect` for every detection but
/// `D009` and `D014`; `rollout-undo` for `D001` to `D005` and `D014`; `raise-memory-limit` for
/// `D003`; `set-image-pull-policy` for `D002`; `delete-evicted-pods` for none.
pub(crate) const ALLOWED: [(Action, &[Detection]); 7] = [
    (Action::None, &DETECTIONS),
    (Action::NeedsHuman, &DETECTIONS),
    (Action::Inspect, &INSPECTED),
    (
        Action::RolloutUndo,
        &[
            Detection::CrashLoop,
            Detection::ImagePull,
            Detection::OutOfMemory,
            Detection::ContainerCreate,
            Detection::Unschedulable,
            Detection::RolloutStalled,
        ],
    ),
    (Action::DeleteEvictedPods, &[]),
    (Action::RaiseMemoryLimit, &[Detection::OutOfMemory]),
    (Action::SetImagePullPolicy, &[Detection::ImagePull]),
];

/// Whether [`ALLOWED`] allows `action` for `detection`.
pub(crate) fn allows(action: Action, detection: Detection) -> bool {
    ALLOWED
        .iter()
        .any(|(allowed, detections)| *allowed == action && detections.contains(&detection))
}

/// `target` as the record names it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    api_version: Box<str>,
    kind: Box<str>,
    name: Box<str>,
    uid: Option<Uid>,
}

impl Target {
    /// Whether it is the object kausa holds: the same apiVersion, kind and name, and the same UID
    /// or none on both sides.
    fn is(&self, held: &Held) -> bool {
        *self.api_version == *held.api_version()
            && *self.kind == *held.kind()
            && *self.name == *held.name()
            && self.uid == held.uid()
    }
}

/// What the parse keeps of a Remediation: the fields it reads, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    namespace: Box<str>,
    /// `<fingerprint>.<firstSeenUnix>`, the end of both names.
    rest: Box<str>,
    incident_uid: Uid,
    detection: Detection,
    fingerprint: Box<str>,
    target: Target,
    /// `None` for an id kausa does not know.
    action: Option<Action>,
    proposed_at: Timestamp,
}

/// The string at `key` of `object`, if there is one.
fn text<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

/// Whether `rest` is `[0-9a-f]{16}\.[0-9]{1,10}`, the end of both names.
fn is_rest(rest: &str) -> bool {
    rest.split_once('.').is_some_and(|(fingerprint, second)| {
        is_fingerprint(fingerprint)
            && (1..=MAX_SECOND_DIGITS).contains(&second.len())
            && second.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The end of a Remediation's name, `<rest>` in `kurita.<rest>`, when the name has that shape.
fn rest_of(name: &str) -> Option<&str> {
    name.strip_prefix(NAME_PREFIX).filter(|rest| is_rest(rest))
}

/// The detection whose id is `id`: the enum of kurita's schema (`:112-128`).
fn detection(id: &str) -> Option<Detection> {
    DETECTIONS
        .into_iter()
        .find(|detection| detection.id() == id)
}

/// `proposedAt`, when it is `YYYY-MM-DDTHH:MM:SSZ` (kurita's schema, `:198-199`) and that is a
/// real instant written as P1's `time` writes one.
fn instant(text: &str) -> Option<Timestamp> {
    if !is_time(text) {
        return None;
    }
    let at: Timestamp = text.parse().ok()?;
    (time(at) == text).then_some(at)
}

fn target(target: &Map<String, Value>) -> Option<Target> {
    let uid = match target.get("uid") {
        Some(uid) => Some(Uid::parse(uid.as_str()?)?),
        None => None,
    };
    Some(Target {
        api_version: text(target, "apiVersion")
            .filter(|v| is_api_version(v))?
            .into(),
        kind: text(target, "kind").filter(|k| is_kind(k))?.into(),
        name: text(target, "name").filter(|n| is_dns_subdomain(n))?.into(),
        uid,
    })
}

/// kausa's strict parse of the fields it reads (check 1); `None` for a record that fails it.
/// Fields it does not read are skipped, whatever they hold.
pub(crate) fn parse(object: &Value) -> Option<Record> {
    let metadata = object.get("metadata")?.as_object()?;
    let namespace = text(metadata, "namespace").filter(|n| is_dns_label(n))?;
    let rest = rest_of(text(metadata, "name")?)?;
    let spec = object.get("spec")?.as_object()?;
    let incident = spec.get("incident")?.as_object()?;
    if text(incident, "name")?.strip_prefix(INCIDENT_PREFIX) != Some(rest) {
        return None;
    }
    let incident_uid = Uid::parse(text(incident, "uid")?)?;
    let detection = detection(text(incident, "detection")?)?;
    let fingerprint = text(incident, "fingerprint").filter(|f| is_fingerprint(f))?;
    let target = target(spec.get("target")?.as_object()?)?;
    let action = text(spec, "action").filter(|a| a.len() <= ACTION_LEN)?;
    if text(spec, "mode")? != MODE {
        return None;
    }
    let proposed_at = instant(text(spec, "proposedAt")?)?;
    Some(Record {
        namespace: namespace.into(),
        rest: rest.into(),
        incident_uid,
        detection,
        fingerprint: fingerprint.into(),
        target,
        action: Action::of(action),
        proposed_at,
    })
}

/// A Remediation to report: its action and `proposedAt`, and the table's entry it is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Report<'a> {
    action: Action,
    proposed_at: Timestamp,
    slot: Slot,
    entry: &'a Entry,
}

impl Report<'_> {
    pub(crate) fn action(&self) -> Action {
        self.action
    }

    pub(crate) fn proposed_at(&self) -> Timestamp {
        self.proposed_at
    }

    /// The key that `Table::mark_reported` takes once the message is queued.
    pub(crate) fn key(&self) -> (Slot, Uid) {
        (self.slot, self.entry.uid())
    }

    /// The `remediation` message: the detection, the object and the fingerprint of kausa's own
    /// entry, the action and the time of the record.
    pub(crate) fn notice(&self) -> notification::Remediation {
        let entry = self.entry;
        let detection = entry.detection();
        let target = entry.target();
        notification::Remediation {
            detection_id: detection.id(),
            detection_name: detection.name(),
            api_version: target.api_version().to_owned(),
            kind: target.kind().to_owned(),
            namespace: entry.namespace().to_owned(),
            name: target.name().to_owned(),
            fingerprint: entry.fingerprint(),
            action: self.action.as_str(),
            time: time(self.proposed_at),
        }
    }
}

/// The result of the checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome<'a> {
    /// Every check passed: send it, count it, mark its entry.
    Report(Report<'a>),
    /// Every check passed and the action is `none`: nothing to send, count or mark.
    Nothing,
    /// A check failed: count it, and log it through [`Limiter`].
    Ignored(Reason),
}

fn ensure(holds: bool, reason: Reason) -> Result<(), Reason> {
    if holds { Ok(()) } else { Err(reason) }
}

/// The checks of the module's documentation, in its order, of the Remediation `object` against
/// the Incidents of `table`, at `now`.
pub(crate) fn check<'a>(object: &Value, table: &'a Table, now: Timestamp) -> Outcome<'a> {
    match evaluate(object, table, now) {
        Ok(Some(report)) => Outcome::Report(report),
        Ok(None) => Outcome::Nothing,
        Err(reason) => Outcome::Ignored(reason),
    }
}

fn evaluate<'a>(
    object: &Value,
    table: &'a Table,
    now: Timestamp,
) -> Result<Option<Report<'a>>, Reason> {
    let record = parse(object).ok_or(Reason::Invalid)?;
    let name = format!("{INCIDENT_PREFIX}{}", record.rest);
    let (slot, entry) = table
        .find(&record.namespace, &name, record.incident_uid)
        .ok_or(Reason::IncidentUnknown)?;
    ensure(
        record.detection == entry.detection() && *record.fingerprint == *entry.fingerprint(),
        Reason::Invalid,
    )?;
    let action = record.action.ok_or(Reason::UnknownAction)?;
    ensure(allows(action, entry.detection()), Reason::Invalid)?;
    ensure(record.target.is(entry.target()), Reason::TargetMismatch)?;
    // The Incident's `firstSeen` is written to the second, and the second of its name is
    // `as_second` of the same instant, from the epoch on.
    ensure(
        entry.first_seen().as_second() <= record.proposed_at.as_second()
            && record.proposed_at <= now,
        Reason::TimeOutOfRange,
    )?;
    ensure(!entry.reported(), Reason::Duplicate)?;
    Ok((action != Action::None).then_some(Report {
        action,
        proposed_at: record.proposed_at,
        slot,
        entry,
    }))
}

/// The fields of one `remediation ignored` line: the reason, and the namespace and the name of
/// the record only when each fits its pattern (a DNS label; `kurita.<rest>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ignored<'a> {
    pub reason: Reason,
    pub namespace: Option<&'a str>,
    pub name: Option<&'a str>,
}

/// At most one `remediation ignored` line per `IGNORED_LOG_PERIOD` for each reason; the
/// occurrences in between are held back and counted.
#[derive(Debug, Default)]
pub(crate) struct Limiter {
    last: [Option<Instant>; Reason::ALL.len()],
    held: [u64; Reason::ALL.len()],
}

impl Limiter {
    /// The line for the record `object`, ignored for `reason` at `now`, when one is due; if so,
    /// the period starts again at `now`. Otherwise the occurrence is counted as held back.
    pub(crate) fn line<'a>(
        &mut self,
        reason: Reason,
        object: &'a Value,
        now: Instant,
    ) -> Option<Ignored<'a>> {
        let last = &mut self.last[reason as usize];
        if last.is_some_and(|at| now.saturating_duration_since(at) < IGNORED_LOG_PERIOD) {
            self.held[reason as usize] = self.held[reason as usize].saturating_add(1);
            return None;
        }
        *last = Some(now);
        let metadata = object.get("metadata").and_then(Value::as_object);
        let field = |key| metadata.and_then(|m| text(m, key));
        Some(Ignored {
            reason,
            namespace: field("namespace").filter(|n| is_dns_label(n)),
            name: field("name").filter(|n| rest_of(n).is_some()),
        })
    }

    /// How many occurrences of `reason` were held back, not logged.
    pub(crate) fn held(&self, reason: Reason) -> u64 {
        self.held[reason as usize]
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use regex::Regex;
    use serde_json::json;

    use super::*;
    use crate::incident_resource::{
        Controller, ControllerKind, FINGERPRINT_PATTERN, Facts, INCIDENT_NAME_PATTERN,
        KIND_PATTERN, Missing, NAME_PATTERN, Object, Sample, Spec, TIME_PATTERN, UID_PATTERN,
    };
    use crate::owner::MAX_NAME_LEN;

    const NAMESPACE: &str = "shop";
    const FP: &str = "5d0c2b7e9a1f4c38";
    const INCIDENT_UID: &str = "00000000-0000-4000-8000-0000000000aa";
    const OTHER_UID: &str = "00000000-0000-4000-8000-0000000000bb";
    const UID_POD: &str = "00000000-0000-4000-8000-000000000001";
    const UID_RS: &str = "00000000-0000-4000-8000-000000000002";
    const UID_OWNER: &str = "00000000-0000-4000-8000-000000000003";
    /// The Incident's first sighting, with a fraction its name and its `firstSeen` drop.
    const FIRST_SEEN: &str = "2026-11-24T10:00:00.750Z";
    const PROPOSED_AT: &str = "2026-11-24T10:00:12Z";
    const NOW: &str = "2026-11-24T10:05:00.500Z";

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a timestamp")
    }

    fn uid(text: &str) -> Uid {
        Uid::parse(text).expect("a UID")
    }

    /// A crash loop of a Deployment's Pods.
    fn spec() -> Spec {
        Spec {
            detection: Detection::CrashLoop,
            fingerprint: FP.into(),
            first_seen: at(FIRST_SEEN),
            settled_at: at("2026-11-24T10:01:00Z"),
            object: Object {
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                name: "web".into(),
                uid: Some(UID_OWNER.into()),
            },
            facts: Facts {
                reason: Some("CrashLoopBackOff"),
                exit_code: Some(1),
                container: Some("app".into()),
                pods_seen: 3,
                pods_seen_is_minimum: false,
                pods_now: 4,
            },
            sample: Some(Sample {
                pod_name: "web-7d4b9c6f5d-abcde".into(),
                pod_uid: UID_POD.into(),
                controller: Some(Controller {
                    kind: ControllerKind::ReplicaSet,
                    uid: UID_RS.into(),
                    revision: Some(7),
                }),
            }),
            deployment: None,
            regarding: None,
            missing: Missing::default(),
        }
    }

    fn table_of(spec: &Spec) -> Table {
        let mut table = Table::default();
        let entry =
            Entry::new(NAMESPACE, spec, uid(INCIDENT_UID), Instant::now()).expect("an entry");
        table
            .insert(entry, Instant::now())
            .expect("room in the table");
        table
    }

    fn table() -> Table {
        table_of(&spec())
    }

    /// `<fingerprint>.<firstSeenUnix>` of the Incident of `spec()`.
    fn rest() -> String {
        format!("{FP}.{}", at(FIRST_SEEN).as_second())
    }

    /// kurita's record of the Incident of `spec()`, proposing a rollback, with the fields kausa
    /// does not read set too.
    fn record() -> Value {
        json!({
            "apiVersion": "kurita.fedeam.com/v1alpha1",
            "kind": "Remediation",
            "metadata": {
                "namespace": NAMESPACE,
                "name": format!("kurita.{}", rest()),
                "uid": "00000000-0000-4000-8000-0000000000cc",
                "labels": {"app.kubernetes.io/name": "kurita"},
            },
            "spec": {
                "incident": {
                    "name": format!("kausa.{}", rest()),
                    "uid": INCIDENT_UID,
                    "detection": "D001",
                    "fingerprint": FP,
                },
                "target": {
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "name": "web",
                    "uid": UID_OWNER,
                },
                "action": "rollout-undo",
                "mode": "proposed",
                "proposedAt": PROPOSED_AT,
                "rolloutUndo": {"fromRevision": 7, "toRevision": 6},
                "preconditions": {"generation": 3},
                "decider": {"rule": "R001", "version": "0.1.0"},
            },
        })
    }

    fn with(mut record: Value, pointer: &str, value: Value) -> Value {
        *record.pointer_mut(pointer).expect("the field exists") = value;
        record
    }

    fn without(mut record: Value, pointer: &str) -> Value {
        let (parent, key) = pointer.rsplit_once('/').expect("a pointer");
        record
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .expect("an object")
            .remove(key)
            .expect("the field exists");
        record
    }

    fn outcome(record: &Value, table: &Table) -> Result<Option<Action>, Reason> {
        match check(record, table, at(NOW)) {
            Outcome::Report(report) => Ok(Some(report.action())),
            Outcome::Nothing => Ok(None),
            Outcome::Ignored(reason) => Err(reason),
        }
    }

    fn reason(record: &Value, table: &Table) -> Reason {
        outcome(record, table).expect_err("ignored")
    }

    #[test]
    fn the_allowed_table_is_the_adr_list() {
        // 0028, point 6, "kausa's copy of the actions each detection allows", written out.
        let ids =
            |detections: &[Detection]| -> Vec<&str> { detections.iter().map(|d| d.id()).collect() };
        let every = vec![
            "D001", "D002", "D003", "D004", "D005", "D006", "D007", "D008", "D009", "D010", "D011",
            "D012", "D013", "D014",
        ];
        let but_9_and_14 = vec![
            "D001", "D002", "D003", "D004", "D005", "D006", "D007", "D008", "D010", "D011", "D012",
            "D013",
        ];
        let expected: [(&str, Vec<&str>); 7] = [
            ("none", every.clone()),
            ("needs-human", every.clone()),
            ("inspect", but_9_and_14),
            (
                "rollout-undo",
                vec!["D001", "D002", "D003", "D004", "D005", "D014"],
            ),
            ("delete-evicted-pods", vec![]),
            ("raise-memory-limit", vec!["D003"]),
            ("set-image-pull-policy", vec!["D002"]),
        ];
        let actual: Vec<(&str, Vec<&str>)> = ALLOWED
            .iter()
            .map(|(action, detections)| (action.as_str(), ids(detections)))
            .collect();
        assert_eq!(actual, expected);
        let listed: Vec<Action> = ALLOWED.iter().map(|(action, _)| *action).collect();
        assert_eq!(
            listed,
            Action::ALL,
            "one row per action, in the enum's order"
        );
    }

    /// The rules of kurita's schema that bound the actions, and its action and detection enums,
    /// copied from `charts/kurita/files/remediation-schema.json` at kurita's commit
    /// `64d8a358d765157dc4b68c08d8f33f9a0fa31646` (`:25-27`, `:57-76`, `:112-127`, `:176-184`).
    const KURITA_RULES: &str = r#"{
      "rules": [
        "has(self.inspect) == (self.action == 'inspect')",
        "self.action != 'rollout-undo' || self.incident.detection in ['D001', 'D002', 'D003', 'D004', 'D005', 'D014']",
        "self.action != 'delete-evicted-pods'",
        "self.action != 'raise-memory-limit' || self.incident.detection == 'D003'",
        "self.action != 'set-image-pull-policy' || self.incident.detection == 'D002'",
        "!has(self.inspect) || (self.inspect.what == 'previous-logs' && self.incident.detection in ['D001', 'D008']) || (self.inspect.what == 'pod-events' && self.incident.detection in ['D002', 'D004', 'D005', 'D006', 'D007']) || (self.inspect.what == 'node' && self.incident.detection in ['D003', 'D010']) || (self.inspect.what == 'quota' && self.incident.detection == 'D011') || (self.inspect.what == 'storage-class' && self.incident.detection == 'D012') || (self.inspect.what == 'metrics-api' && self.incident.detection == 'D013')"
      ],
      "actions": ["none", "needs-human", "inspect", "rollout-undo", "delete-evicted-pods",
                  "raise-memory-limit", "set-image-pull-policy"],
      "detections": ["D001", "D002", "D003", "D004", "D005", "D006", "D007", "D008", "D009",
                     "D010", "D011", "D012", "D013", "D014"]
    }"#;

    /// The detections each action is allowed for, read from kurita's rules: an action with a rule
    /// `self.action != '<id>' || <detections>` is allowed for those detections, one with the rule
    /// `self.action != '<id>'` alone for none, `inspect` for the detections of its `inspect.what`
    /// rule, and an action with no rule for every detection.
    fn derived() -> Vec<(String, BTreeSet<String>)> {
        let fixture: Value = serde_json::from_str(KURITA_RULES).expect("the fixture parses");
        let strings = |key: &str| -> Vec<String> {
            fixture[key]
                .as_array()
                .expect("an array")
                .iter()
                .map(|v| v.as_str().expect("a string").to_owned())
                .collect()
        };
        let rules = strings("rules");
        let every: BTreeSet<String> = strings("detections").into_iter().collect();
        let id = Regex::new(r"'(D[0-9]{3})'").expect("a regex");
        let ids = |text: &str| -> BTreeSet<String> {
            id.captures_iter(text).map(|c| c[1].to_owned()).collect()
        };
        assert!(rules.contains(&"has(self.inspect) == (self.action == 'inspect')".to_owned()));
        strings("actions")
            .into_iter()
            .map(|action| {
                let guard = format!("self.action != '{action}'");
                let allowed = if action == "inspect" {
                    let rule = rules
                        .iter()
                        .find(|r| r.starts_with("!has(self.inspect) || "))
                        .expect("the inspect rule");
                    ids(rule)
                } else {
                    match rules.iter().find(|r| r.starts_with(&guard)) {
                        Some(rule) if *rule == guard => BTreeSet::new(),
                        Some(rule) => {
                            let rest = rule
                                .strip_prefix(&format!("{guard} || "))
                                .expect("a guard and the detections");
                            ids(rest)
                        }
                        None => every.clone(),
                    }
                };
                (action, allowed)
            })
            .collect()
    }

    #[test]
    fn the_allowed_table_equals_kuritas_cel_rules() {
        let ours: Vec<(String, BTreeSet<String>)> = ALLOWED
            .iter()
            .map(|(action, detections)| {
                (
                    action.as_str().to_owned(),
                    detections.iter().map(|d| d.id().to_owned()).collect(),
                )
            })
            .collect();
        assert_eq!(ours, derived());
    }

    #[test]
    fn allows_reads_the_table() {
        for (action, detections) in ALLOWED {
            for detection in DETECTIONS {
                assert_eq!(
                    allows(action, detection),
                    detections.contains(&detection),
                    "{action:?} {detection:?}"
                );
            }
        }
    }

    #[test]
    fn the_reasons_have_their_labels() {
        let labels: Vec<&str> = Reason::ALL.into_iter().map(Reason::as_str).collect();
        assert_eq!(
            labels,
            [
                "incident-unknown",
                "invalid",
                "unknown-action",
                "target-mismatch",
                "time-out-of-range",
                "duplicate",
            ]
        );
        for (i, reason) in Reason::ALL.into_iter().enumerate() {
            assert_eq!(
                reason as usize, i,
                "the limiter indexes by the discriminant"
            );
        }
    }

    #[test]
    fn a_record_that_passes_every_check_is_reported_from_the_entry() {
        let table = table();
        let Outcome::Report(report) = check(&record(), &table, at(NOW)) else {
            panic!("a report");
        };
        assert_eq!(report.action(), Action::RolloutUndo);
        assert_eq!(report.proposed_at(), at(PROPOSED_AT));
        let (slot, incident) = table
            .find(NAMESPACE, &format!("kausa.{}", rest()), uid(INCIDENT_UID))
            .map(|(slot, entry)| (slot, entry.uid()))
            .expect("the entry");
        assert_eq!(report.key(), (slot, incident));
        assert_eq!(
            report.notice(),
            notification::Remediation {
                detection_id: "D001",
                detection_name: "crash loop",
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                namespace: NAMESPACE.into(),
                name: "web".into(),
                fingerprint: FP.into(),
                action: "rollout-undo",
                time: PROPOSED_AT.into(),
            }
        );
    }

    #[test]
    fn the_check_marks_nothing_and_the_caller_marks_the_entry() {
        let mut table = table();
        let key = match check(&record(), &table, at(NOW)) {
            Outcome::Report(report) => report.key(),
            other => panic!("{other:?}"),
        };
        // The check takes the table by shared reference: a second evaluation is still a report.
        assert!(matches!(
            check(&record(), &table, at(NOW)),
            Outcome::Report(_)
        ));
        assert!(table.mark_reported(key.0, key.1));
        assert_eq!(reason(&record(), &table), Reason::Duplicate);
    }

    #[test]
    fn fields_kausa_does_not_read_leave_the_parse_unchanged() {
        let one = record();
        let mut other = record();
        let spec = other["spec"].as_object_mut().expect("the spec");
        spec.insert("reason".into(), json!("no-safe-action"));
        spec.insert(
            "rolloutUndo".into(),
            json!({"fromRevision": 9, "toRevision": 1}),
        );
        spec.insert("preconditions".into(), json!({"generation": 99}));
        spec.insert(
            "decider".into(),
            json!({"rule": "R999", "version": "9.9.9"}),
        );
        spec.insert("unknownField".into(), json!(["anything", 1, null]));
        let metadata = other["metadata"].as_object_mut().expect("the metadata");
        metadata.insert("uid".into(), json!(OTHER_UID));
        metadata.insert("annotations".into(), json!({"x": "y"}));
        other["status"] = json!({"anything": true});
        other["kind"] = json!("NotEvenRemediation");
        assert_eq!(parse(&one), parse(&other));
        assert!(parse(&one).is_some());
    }

    #[test]
    fn unknown_fields_are_skipped() {
        let mut extra = record();
        extra["spec"]["incident"]["extra"] = json!({"deep": [1, 2, 3]});
        extra["spec"]["target"]["extra"] = json!("x");
        extra["metadata"]["extra"] = json!(1);
        extra["extra"] = json!(null);
        assert_eq!(parse(&extra), parse(&record()));
        assert_eq!(outcome(&extra, &table()), Ok(Some(Action::RolloutUndo)));
    }

    #[test]
    fn a_record_missing_a_field_it_reads_is_invalid() {
        for pointer in [
            "/metadata",
            "/metadata/namespace",
            "/metadata/name",
            "/spec",
            "/spec/incident",
            "/spec/incident/name",
            "/spec/incident/uid",
            "/spec/incident/detection",
            "/spec/incident/fingerprint",
            "/spec/target",
            "/spec/target/apiVersion",
            "/spec/target/kind",
            "/spec/target/name",
            "/spec/action",
            "/spec/mode",
            "/spec/proposedAt",
        ] {
            assert_eq!(
                reason(&without(record(), pointer), &table()),
                Reason::Invalid,
                "{pointer}"
            );
        }
        assert_eq!(reason(&json!(null), &table()), Reason::Invalid);
        assert_eq!(reason(&json!("kurita"), &table()), Reason::Invalid);
        assert_eq!(reason(&json!({}), &table()), Reason::Invalid);
    }

    #[test]
    fn a_field_of_another_type_is_invalid() {
        for pointer in [
            "/metadata",
            "/metadata/namespace",
            "/metadata/name",
            "/spec",
            "/spec/incident",
            "/spec/incident/name",
            "/spec/incident/uid",
            "/spec/incident/detection",
            "/spec/incident/fingerprint",
            "/spec/target",
            "/spec/target/apiVersion",
            "/spec/target/kind",
            "/spec/target/name",
            "/spec/target/uid",
            "/spec/action",
            "/spec/mode",
            "/spec/proposedAt",
        ] {
            for value in [json!(null), json!(1), json!(true), json!([]), json!({})] {
                // An empty object in place of an object fails on its missing fields.
                let r = with(record(), pointer, value.clone());
                assert_eq!(reason(&r, &table()), Reason::Invalid, "{pointer} = {value}");
            }
        }
    }

    #[test]
    fn a_mode_other_than_proposed_is_invalid() {
        for mode in ["executed", "Proposed", "proposed ", "", "propose"] {
            assert_eq!(
                reason(&with(record(), "/spec/mode", json!(mode)), &table()),
                Reason::Invalid,
                "{mode}"
            );
        }
    }

    #[test]
    fn the_names_must_be_kurita_and_kausa_of_the_same_rest() {
        let other_rest = format!("{FP}.{}", at(FIRST_SEEN).as_second() + 1);
        for (name, incident) in [
            // Not `kurita.` or not `kausa.`.
            (format!("kausa.{}", rest()), format!("kausa.{}", rest())),
            (format!("kurita.{}", rest()), format!("kurita.{}", rest())),
            (format!("Kurita.{}", rest()), format!("kausa.{}", rest())),
            // Two different rests.
            (format!("kurita.{}", rest()), format!("kausa.{other_rest}")),
            (format!("kurita.{other_rest}"), format!("kausa.{}", rest())),
            // A rest of another shape.
            (format!("kurita.{FP}"), format!("kausa.{FP}")),
            (
                "kurita.5D0C2B7E9A1F4C38.1".into(),
                "kausa.5D0C2B7E9A1F4C38.1".into(),
            ),
            (
                format!("kurita.{FP}.12345678901"),
                format!("kausa.{FP}.12345678901"),
            ),
            (format!("kurita.{FP}."), format!("kausa.{FP}.")),
            (format!("kurita.{FP}.1a"), format!("kausa.{FP}.1a")),
            (format!("kurita.{FP}.1.2"), format!("kausa.{FP}.1.2")),
        ] {
            let r = with(
                with(record(), "/metadata/name", json!(name)),
                "/spec/incident/name",
                json!(incident),
            );
            assert_eq!(reason(&r, &table()), Reason::Invalid, "{name} {incident}");
        }
    }

    #[test]
    fn an_api_version_that_fails_is_api_version_is_invalid() {
        // Values that fail kausa's pattern and kurita's both (the ruling of 2026-11-25).
        for api_version in [
            "Apps/v1".to_owned(),
            "apps/V1".to_owned(),
            "apps /v1".to_owned(),
            "apps/v 1".to_owned(),
            "apps//v1".to_owned(),
            "a/b/v1".to_owned(),
            String::new(),
            format!("{}/v1", "a".repeat(MAX_NAME_LEN)),
        ] {
            assert!(!is_api_version(&api_version), "{api_version}");
            let r = with(record(), "/spec/target/apiVersion", json!(api_version));
            assert_eq!(reason(&r, &table()), Reason::Invalid, "{api_version}");
        }
    }

    #[test]
    fn each_field_is_checked_against_kuritas_pattern_and_length() {
        // (pointer, kurita's pattern, kurita's maxLength), from its schema at
        // 64d8a358d765157dc4b68c08d8f33f9a0fa31646: `:100-101`, `:106-107`, `:133-134`,
        // `:156-157`, `:162-163`, `:168-169`; `proposedAt` (`:198-199`) only for its shape, since
        // a value of that shape may still not be a date.
        let fields: [(&str, &str, usize); 6] = [
            (
                "/spec/incident/uid",
                "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
                36,
            ),
            ("/spec/incident/fingerprint", "^[0-9a-f]{16}$", 16),
            ("/spec/target/kind", "^[A-Z][A-Za-z0-9]*$", 63),
            (
                "/spec/target/name",
                "^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$",
                253,
            ),
            (
                "/spec/target/uid",
                "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
                36,
            ),
            (
                "/spec/proposedAt",
                "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}Z$",
                20,
            ),
        ];
        // The same patterns as kausa's own, so an Incident's values pass them.
        assert_eq!(fields[0].1, UID_PATTERN);
        assert_eq!(fields[1].1, FINGERPRINT_PATTERN);
        assert_eq!(fields[2].1, KIND_PATTERN);
        assert_eq!(fields[3].1, NAME_PATTERN);
        assert_eq!(
            fields[5].1.replace("\\d", "[0-9]"),
            TIME_PATTERN,
            "\\d is [0-9] in RE2"
        );
        let candidates = [
            "5d0c2b7e9a1f4c38",
            "5d0c2b7e9a1f4c3",
            "5d0c2b7e9a1f4c381",
            "5D0C2B7E9A1F4C38",
            UID_OWNER,
            "00000000-0000-4000-8000-00000000000A",
            "00000000000040008000000000000003",
            "Deployment",
            "deployment",
            "D",
            "Deploy-ment",
            "web",
            "web.example.com",
            "web..example",
            "-web",
            "web-",
            "a.b-c.d",
            "2026-11-24T10:00:12Z",
            "2026-11-24 10:00:12Z",
            "2026-11-24T10:00:12.000Z",
            "",
            " ",
            "\u{661}",
        ];
        let long = [
            "a".repeat(253),
            "a".repeat(254),
            format!("A{}", "a".repeat(62)),
            format!("A{}", "a".repeat(63)),
        ];
        for (pointer, pattern, max) in fields {
            let regex = Regex::new(pattern).expect("a regex");
            for candidate in candidates
                .iter()
                .copied()
                .chain(long.iter().map(String::as_str))
            {
                let fits = regex.is_match(candidate) && candidate.len() <= max;
                let parsed = parse(&with(record(), pointer, json!(candidate))).is_some();
                if pointer == "/spec/proposedAt" {
                    assert!(parsed <= fits, "{pointer}: {candidate}");
                    continue;
                }
                assert_eq!(parsed, fits, "{pointer}: {candidate:?}");
            }
        }
        // incident.name, `:100-101`, is the same pattern as kausa's Incident name.
        let regex = Regex::new(INCIDENT_NAME_PATTERN).expect("a regex");
        assert_eq!(
            INCIDENT_NAME_PATTERN,
            "^kausa\\.[0-9a-f]{16}\\.[0-9]{1,10}$"
        );
        for second in ["0", "1", "0123", "9999999999", "10000000000", "", "1a"] {
            let name = format!("kausa.{FP}.{second}");
            let r = with(
                with(
                    record(),
                    "/metadata/name",
                    json!(format!("kurita.{FP}.{second}")),
                ),
                "/spec/incident/name",
                json!(name),
            );
            assert_eq!(
                parse(&r).is_some(),
                regex.is_match(&name) && name.len() <= 33,
                "{name}"
            );
        }
    }

    #[test]
    fn a_detection_outside_kuritas_enum_is_invalid() {
        for detection in ["D000", "D015", "d001", "D1", "D0001", "crash loop", ""] {
            let r = with(record(), "/spec/incident/detection", json!(detection));
            assert_eq!(reason(&r, &table()), Reason::Invalid, "{detection}");
        }
    }

    #[test]
    fn proposed_at_must_be_a_real_instant_in_the_exact_form() {
        for text in [
            "2026-02-30T10:00:12Z",
            "2026-13-01T10:00:12Z",
            "2026-11-24T24:00:00Z",
            "2026-11-24T10:60:00Z",
            "2026-11-24T10:00:60Z",
            "2026-11-24T10:00:12+00:00",
            "2026-11-24t10:00:12z",
            "2026-11-24T10:00:12.5Z",
            "20261124T100012Z",
        ] {
            let r = with(record(), "/spec/proposedAt", json!(text));
            assert_eq!(reason(&r, &table()), Reason::Invalid, "{text}");
        }
    }

    #[test]
    fn a_record_of_an_incident_not_in_the_table_is_unknown() {
        let other_namespace = with(record(), "/metadata/namespace", json!("other"));
        let other_uid = with(record(), "/spec/incident/uid", json!(OTHER_UID));
        let next = format!("{FP}.{}", at(FIRST_SEEN).as_second() + 1);
        let other_name = with(
            with(record(), "/metadata/name", json!(format!("kurita.{next}"))),
            "/spec/incident/name",
            json!(format!("kausa.{next}")),
        );
        for r in [other_namespace, other_uid, other_name] {
            assert_eq!(reason(&r, &table()), Reason::IncidentUnknown, "{r}");
        }
        assert_eq!(
            reason(&record(), &Table::default()),
            Reason::IncidentUnknown
        );
    }

    #[test]
    fn a_detection_or_fingerprint_other_than_the_entrys_is_invalid() {
        let r = with(record(), "/spec/incident/detection", json!("D003"));
        assert_eq!(reason(&r, &table()), Reason::Invalid);
        let r = with(
            record(),
            "/spec/incident/fingerprint",
            json!("0000000000000000"),
        );
        assert_eq!(reason(&r, &table()), Reason::Invalid);
    }

    #[test]
    fn an_action_kausa_does_not_know_is_unknown_and_a_longer_one_invalid() {
        for action in ["scale-up", "Rollout-Undo", "", "x", "a-twenty-one-chars-id"] {
            assert!(action.len() <= ACTION_LEN);
            let r = with(record(), "/spec/action", json!(action));
            assert_eq!(reason(&r, &table()), Reason::UnknownAction, "{action}");
        }
        let r = with(record(), "/spec/action", json!("a-twenty-two-chars-idx"));
        assert_eq!(reason(&r, &table()), Reason::Invalid);
    }

    #[test]
    fn an_action_not_allowed_for_the_detection_is_invalid() {
        for action in Action::ALL {
            let r = with(record(), "/spec/action", json!(action.as_str()));
            let expected = if allows(action, Detection::CrashLoop) {
                Ok((action != Action::None).then_some(action))
            } else {
                Err(Reason::Invalid)
            };
            assert_eq!(outcome(&r, &table()), expected, "{action:?}");
        }
    }

    #[test]
    fn the_target_must_be_the_entrys_object() {
        for (pointer, value) in [
            ("/spec/target/apiVersion", json!("apps/v2")),
            ("/spec/target/apiVersion", json!("v1")),
            ("/spec/target/kind", json!("StatefulSet")),
            ("/spec/target/name", json!("web2")),
            ("/spec/target/uid", json!(OTHER_UID)),
        ] {
            let r = with(record(), pointer, value.clone());
            assert_eq!(
                reason(&r, &table()),
                Reason::TargetMismatch,
                "{pointer} {value}"
            );
        }
        // A UID on one side only.
        let r = without(record(), "/spec/target/uid");
        assert_eq!(reason(&r, &table()), Reason::TargetMismatch);
        let mut no_uid = spec();
        no_uid.object.uid = None;
        let table = table_of(&no_uid);
        assert_eq!(reason(&record(), &table), Reason::TargetMismatch);
        // None on both sides.
        assert_eq!(
            outcome(&without(record(), "/spec/target/uid"), &table),
            Ok(Some(Action::RolloutUndo))
        );
    }

    #[test]
    fn proposed_at_is_within_first_seen_and_now_both_included() {
        let table = table();
        let first = at("2026-11-24T10:00:00Z");
        let now = at(NOW);
        let report_at = |proposed: Timestamp, now: Timestamp| {
            let r = with(record(), "/spec/proposedAt", json!(time(proposed)));
            match check(&r, &table, now) {
                Outcome::Report(_) => Ok(()),
                Outcome::Ignored(reason) => Err(reason),
                Outcome::Nothing => panic!("not none"),
            }
        };
        // `firstSeen` is 10:00:00.750: its second, 10:00:00, is in range, the one before is not.
        assert_eq!(report_at(first, now), Ok(()));
        let second = k8s_openapi::jiff::SignedDuration::from_secs(1);
        assert_eq!(report_at(first - second, now), Err(Reason::TimeOutOfRange));
        // `now` itself is in range, a second after it is not.
        let now_second = at("2026-11-24T10:05:00Z");
        assert_eq!(report_at(now_second, now_second), Ok(()));
        assert_eq!(report_at(now_second, now), Ok(()));
        assert_eq!(
            report_at(now_second + second, now),
            Err(Reason::TimeOutOfRange)
        );
        assert_eq!(
            report_at(now_second, now_second - second),
            Err(Reason::TimeOutOfRange)
        );
    }

    #[test]
    fn none_is_neither_reported_nor_ignored_and_marks_nothing() {
        let table = table();
        let mut none = with(record(), "/spec/action", json!("none"));
        none["spec"]["reason"] = json!("not-reproduced");
        assert_eq!(check(&none, &table, at(NOW)), Outcome::Nothing);
        // Nothing was marked: kurita's next record of the Incident is still reported.
        let human = with(record(), "/spec/action", json!("needs-human"));
        assert_eq!(outcome(&human, &table), Ok(Some(Action::NeedsHuman)));
        // A `none` that fails a check is ignored like any other record.
        let wrong = with(none.clone(), "/spec/target/name", json!("other"));
        assert_eq!(reason(&wrong, &table), Reason::TargetMismatch);
        let mut marked = self::table();
        let (slot, incident) = match check(&record(), &marked, at(NOW)) {
            Outcome::Report(report) => report.key(),
            other => panic!("{other:?}"),
        };
        assert!(marked.mark_reported(slot, incident));
        assert_eq!(reason(&none, &marked), Reason::Duplicate);
    }

    #[test]
    fn the_first_failing_check_names_the_reason() {
        let table = table();
        let unknown_uid = |r: Value| with(r, "/spec/incident/uid", json!(OTHER_UID));
        // 1 and 2: an invalid record of an unknown Incident.
        let r = unknown_uid(with(record(), "/spec/mode", json!("executed")));
        assert_eq!(reason(&r, &table), Reason::Invalid);
        // 2 and 3: an unknown Incident whose detection differs too.
        let r = unknown_uid(with(record(), "/spec/incident/detection", json!("D003")));
        assert_eq!(reason(&r, &table), Reason::IncidentUnknown);
        // 3 and 4: another fingerprint and an unknown action.
        let r = with(
            with(
                record(),
                "/spec/incident/fingerprint",
                json!("0000000000000000"),
            ),
            "/spec/action",
            json!("scale-up"),
        );
        assert_eq!(reason(&r, &table), Reason::Invalid);
        // 4 and 6: an unknown action and another target.
        let r = with(
            with(record(), "/spec/action", json!("scale-up")),
            "/spec/target/name",
            json!("other"),
        );
        assert_eq!(reason(&r, &table), Reason::UnknownAction);
        // 5 and 6: an action not allowed and another target.
        let r = with(
            with(record(), "/spec/action", json!("raise-memory-limit")),
            "/spec/target/name",
            json!("other"),
        );
        assert_eq!(reason(&r, &table), Reason::Invalid);
        // 6 and 7: another target and a time out of range.
        let r = with(
            with(record(), "/spec/target/name", json!("other")),
            "/spec/proposedAt",
            json!("2026-11-24T09:00:00Z"),
        );
        assert_eq!(reason(&r, &table), Reason::TargetMismatch);
        // 7 and 8: a time out of range for an Incident already reported.
        let mut marked = self::table();
        let (slot, incident) = match check(&record(), &marked, at(NOW)) {
            Outcome::Report(report) => report.key(),
            other => panic!("{other:?}"),
        };
        assert!(marked.mark_reported(slot, incident));
        let r = with(record(), "/spec/proposedAt", json!("2026-11-24T09:00:00Z"));
        assert_eq!(reason(&r, &marked), Reason::TimeOutOfRange);
        assert_eq!(reason(&record(), &marked), Reason::Duplicate);
    }

    #[test]
    fn the_limiter_logs_once_a_minute_per_reason_and_counts_the_others() {
        let mut limiter = Limiter::default();
        let t0 = Instant::now();
        let r = record();
        let name = format!("kurita.{}", rest());
        let line = limiter
            .line(Reason::Invalid, &r, t0)
            .expect("the first line");
        assert_eq!(
            line,
            Ignored {
                reason: Reason::Invalid,
                namespace: Some(NAMESPACE),
                name: Some(name.as_str()),
            }
        );
        // Another reason has its own minute.
        assert!(limiter.line(Reason::Duplicate, &r, t0).is_some());
        let just_before = t0 + IGNORED_LOG_PERIOD - Duration::from_millis(1);
        assert_eq!(limiter.line(Reason::Invalid, &r, t0), None);
        assert_eq!(limiter.line(Reason::Invalid, &r, just_before), None);
        assert_eq!(limiter.held(Reason::Invalid), 2);
        assert_eq!(limiter.held(Reason::Duplicate), 0);
        // A minute after the last line, exactly, the next one is logged, and the period restarts.
        let minute = t0 + IGNORED_LOG_PERIOD;
        assert!(limiter.line(Reason::Invalid, &r, minute).is_some());
        assert_eq!(limiter.line(Reason::Invalid, &r, minute), None);
        assert_eq!(limiter.held(Reason::Invalid), 3);
    }

    #[test]
    fn the_limiter_gives_the_namespace_and_name_only_when_they_fit() {
        let t0 = Instant::now();
        let fits = format!("kurita.{FP}.1");
        for (namespace, name, expected) in [
            (json!("Shop"), json!("kurita.x"), (None, None)),
            (json!(1), json!(null), (None, None)),
            (
                json!("a".repeat(64)),
                json!(format!("kurita.{FP}")),
                (None, None),
            ),
            (
                json!("shop"),
                json!(format!("kurita.{FP}.1\n")),
                (Some("shop"), None),
            ),
            (
                json!("shop\u{202e}"),
                json!(format!("kurita.{FP}.1")),
                (None, Some(fits.as_str())),
            ),
        ] {
            let mut limiter = Limiter::default();
            let r = json!({"metadata": {"namespace": namespace, "name": name}});
            let line = limiter.line(Reason::Invalid, &r, t0).expect("a line");
            assert_eq!((line.namespace, line.name), expected, "{r}");
        }
        let mut limiter = Limiter::default();
        let array = json!([1]);
        let line = limiter.line(Reason::Invalid, &array, t0).expect("a line");
        assert_eq!((line.namespace, line.name), (None, None));
    }

    #[test]
    fn context_uses_neither_incidents_nor_remediations() {
        // 0028, point 8: nothing read back from an Incident or a Remediation reaches the model.
        let context = include_str!("context.rs");
        for module in ["remediation", "incident_resource"] {
            assert!(!context.contains(module), "src/context.rs names {module}");
        }
    }
}
