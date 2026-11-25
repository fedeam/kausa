//! The Incident's `spec` and name (docs/adr/0028-incident-resource-and-remediation-report.md,
//! points 1 and 2): what kausa would send in the one `create` of an Incident, and the check that
//! it is never a create the schema in `charts/kausa/files/incident-schema.json` would refuse.
//!
//! Pure: no I/O, no clock, no state. The check applies the schema's patterns, lengths, ranges,
//! enums and CEL rules, and the rule for the name, in Rust; tests tie each of them to the file. A
//! refusal names the first field that does not fit, from a closed list, never its value.

use k8s_openapi::jiff::Timestamp;
use serde_json::{Map, Value, json};

use crate::detect::Detection;
use crate::fact;
use crate::incident::MAX_PODS;
use crate::owner::{MAX_NAME_LEN, MAX_NAMESPACE_LEN, is_dns_label, is_dns_subdomain};

pub(crate) mod table;

pub(crate) use crate::record::{UID_LEN, Uid};
pub(crate) use crate::report::{ControllerKind, REGARDING_KINDS};

/// The 16 lowercase hexadecimal characters of a fingerprint.
pub(crate) const FINGERPRINT_PATTERN: &str = "^[0-9a-f]{16}$";
pub(crate) const FINGERPRINT_LEN: usize = 16;

/// A time in UTC to the second, as kausa writes it: `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) const TIME_PATTERN: &str = "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$";
pub(crate) const TIME_LEN: usize = 20;

/// A UID as the API server sets one: a UUID in lowercase hexadecimal.
pub(crate) const UID_PATTERN: &str =
    "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

/// A DNS-1123 subdomain, of at most `MAX_NAME_LEN` characters: `is_dns_subdomain`.
pub(crate) const NAME_PATTERN: &str =
    "^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$";

/// A DNS-1123 label, of at most `MAX_NAMESPACE_LEN` characters: `is_dns_label`.
pub(crate) const LABEL_PATTERN: &str = "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$";

/// A kind, of at most `KIND_LEN` characters.
pub(crate) const KIND_PATTERN: &str = "^[A-Z][A-Za-z0-9]*$";
pub(crate) const KIND_LEN: usize = 63;

/// An apiVersion, of at most `MAX_NAME_LEN` characters: a group that is a DNS-1123 subdomain and
/// a `/` before a version, or a version alone, as the core group's `v1`. The version starts with a
/// letter and has no limit of its own: kurita's pattern, byte for byte.
pub(crate) const API_VERSION_PATTERN: &str = concat!(
    "^([a-z0-9]([-a-z0-9]*[a-z0-9])?(\\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*/)?",
    "[a-z]([-a-z0-9]*[a-z0-9])?$"
);

/// The Incident's name: `kausa.`, the fingerprint, `.` and the first-seen time in Unix seconds, of
/// 1 to 10 digits, the pattern kurita's record names an Incident by.
pub(crate) const INCIDENT_NAME_PATTERN: &str = "^kausa\\.[0-9a-f]{16}\\.[0-9]{1,10}$";

/// The first first-seen time the name cannot hold: 10^10 seconds, 2286-11-20T17:46:40Z.
pub(crate) const NAME_SECONDS_END: i64 = 10_000_000_000;

/// The most Pods `podsSeen` holds; a count above it is written as this, with `podsSeenIsMinimum`.
pub(crate) const MAX_PODS_SEEN: usize = MAX_PODS;

/// The most Pods `podsNow` may say; above it, no create.
pub(crate) const MAX_PODS_NOW: usize = 1_000_000;

/// The highest revision: 2^53, the largest integer every JSON reader holds exactly.
pub(crate) const MAX_REVISION: u64 = crate::record::MAX_REVISION;

/// The detections, in the order of their ids.
pub(crate) const DETECTIONS: [Detection; 14] = [
    Detection::CrashLoop,
    Detection::ImagePull,
    Detection::OutOfMemory,
    Detection::ContainerCreate,
    Detection::Unschedulable,
    Detection::ProbeFailure,
    Detection::VolumeFailure,
    Detection::JobFailed,
    Detection::PodEvicted,
    Detection::SandboxCreate,
    Detection::PodCreate,
    Detection::ClaimProvisioning,
    Detection::AutoscalerMetrics,
    Detection::RolloutStalled,
];

/// The schema's CEL rules, as the file writes them, each with the field a refusal of its Rust
/// mirror names. On `spec`:
pub(crate) const RULE_IMMUTABLE: &str = "self == oldSelf";
pub(crate) const RULE_SAMPLE: &str = "has(self.sample) == (self.facts.podsSeen > 0)";
pub(crate) const RULE_PODS_SEEN: &str = "(self.facts.podsSeen == 0) == \
     (self.detection in ['D008', 'D011', 'D012', 'D013', 'D014'])";
pub(crate) const RULE_DEPLOYMENT: &str = "has(self.deployment) == (self.detection == 'D014')";
pub(crate) const RULE_MISSING_REVISION: &str = "('revision-lookup-failed' in self.missing) == \
     (has(self.sample) && has(self.sample.controller) && \
     self.sample.controller.kind == 'ReplicaSet' && !has(self.sample.controller.revision))";
pub(crate) const RULE_SAMPLE_OR_REGARDING: &str = "!(has(self.sample) && has(self.regarding))";
pub(crate) const RULE_MISSING_OWNER: &str = "!('owner-lookup-failed' in self.missing) || \
     has(self.sample) || self.detection in ['D008', 'D011']";
pub(crate) const RULE_OWNER_LOOKUP_OBJECT: &str = "!('owner-lookup-failed' in self.missing && \
     self.detection in ['D008', 'D011']) || (has(self.regarding) && \
     self.object.kind == self.regarding.kind && \
     self.object.name == self.regarding.name && has(self.object.uid) == has(self.regarding.uid) && \
     (!has(self.object.uid) || self.object.uid == self.regarding.uid))";
/// On `facts`:
pub(crate) const RULE_MINIMUM: &str = "self.podsSeenIsMinimum == (self.podsSeen == 100)";
/// On `sample.controller`:
pub(crate) const RULE_CONTROLLER_REVISION: &str =
    "!has(self.revision) || self.kind == 'ReplicaSet'";

/// The field a refusal names: a closed list, so a refusal says where, never what.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Field {
    Name,
    Fingerprint,
    FirstSeen,
    SettledAt,
    ObjectApiVersion,
    ObjectKind,
    ObjectName,
    ObjectUid,
    FactsReason,
    FactsContainer,
    FactsPodsSeen,
    FactsPodsSeenIsMinimum,
    FactsPodsNow,
    Sample,
    SamplePodName,
    SamplePodUid,
    SampleControllerUid,
    SampleControllerRevision,
    Deployment,
    DeploymentGeneration,
    DeploymentRevision,
    RegardingKind,
    RegardingName,
    RegardingUid,
    Regarding,
    Object,
    Missing,
}

impl Field {
    pub(crate) const ALL: [Field; 27] = [
        Field::Name,
        Field::Fingerprint,
        Field::FirstSeen,
        Field::SettledAt,
        Field::ObjectApiVersion,
        Field::ObjectKind,
        Field::ObjectName,
        Field::ObjectUid,
        Field::FactsReason,
        Field::FactsContainer,
        Field::FactsPodsSeen,
        Field::FactsPodsSeenIsMinimum,
        Field::FactsPodsNow,
        Field::Sample,
        Field::SamplePodName,
        Field::SamplePodUid,
        Field::SampleControllerUid,
        Field::SampleControllerRevision,
        Field::Deployment,
        Field::DeploymentGeneration,
        Field::DeploymentRevision,
        Field::RegardingKind,
        Field::RegardingName,
        Field::RegardingUid,
        Field::Regarding,
        Field::Object,
        Field::Missing,
    ];

    /// The field's path in the object: `metadata.name`, or its path under `spec`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Field::Name => "metadata.name",
            Field::Fingerprint => "fingerprint",
            Field::FirstSeen => "firstSeen",
            Field::SettledAt => "settledAt",
            Field::ObjectApiVersion => "object.apiVersion",
            Field::ObjectKind => "object.kind",
            Field::ObjectName => "object.name",
            Field::ObjectUid => "object.uid",
            Field::FactsReason => "facts.reason",
            Field::FactsContainer => "facts.container",
            Field::FactsPodsSeen => "facts.podsSeen",
            Field::FactsPodsSeenIsMinimum => "facts.podsSeenIsMinimum",
            Field::FactsPodsNow => "facts.podsNow",
            Field::Sample => "sample",
            Field::SamplePodName => "sample.pod.name",
            Field::SamplePodUid => "sample.pod.uid",
            Field::SampleControllerUid => "sample.controller.uid",
            Field::SampleControllerRevision => "sample.controller.revision",
            Field::Deployment => "deployment",
            Field::DeploymentGeneration => "deployment.generation",
            Field::DeploymentRevision => "deployment.revision",
            Field::RegardingKind => "regarding.kind",
            Field::RegardingName => "regarding.name",
            Field::RegardingUid => "regarding.uid",
            Field::Regarding => "regarding",
            Field::Object => "object",
            Field::Missing => "missing",
        }
    }
}

/// The values of `missing`, in the order the Incident lists them.
pub(crate) const MISSING_VALUES: [&str; 2] = ["owner-lookup-failed", "revision-lookup-failed"];

/// Which of the two reads failed (point 3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Missing {
    pub owner_lookup_failed: bool,
    pub revision_lookup_failed: bool,
}

impl Missing {
    fn values(self) -> Vec<&'static str> {
        [self.owner_lookup_failed, self.revision_lookup_failed]
            .into_iter()
            .zip(MISSING_VALUES)
            .filter_map(|(set, value)| set.then_some(value))
            .collect()
    }
}

/// The incident's top-level owner, as the incident names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Object {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub uid: Option<String>,
}

/// 0027's point 5 vocabulary but its revision, which `deployment` holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    pub reason: Option<&'static str>,
    pub exit_code: Option<i32>,
    pub container: Option<String>,
    pub pods_seen: usize,
    pub pods_seen_is_minimum: bool,
    pub pods_now: usize,
}

impl Facts {
    /// The facts of a fact statement, with its count of Pods seen capped at `MAX_PODS_SEEN`: the
    /// statement's own flag says "at least" from that count on.
    pub(crate) fn of(facts: &fact::Facts) -> Facts {
        Facts {
            reason: facts.reason,
            exit_code: facts.exit_code,
            container: facts.container.clone(),
            pods_seen: facts.pods_seen.min(MAX_PODS_SEEN),
            pods_seen_is_minimum: facts.pods_seen_is_minimum,
            pods_now: facts.pods_now,
        }
    }
}

/// The sample Pod and, when it has a controller `ownerReference`, that controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sample {
    pub pod_name: String,
    pub pod_uid: String,
    pub controller: Option<Controller>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Controller {
    pub kind: ControllerKind,
    pub uid: String,
    /// A ReplicaSet's revision, as the fact slot read it.
    pub revision: Option<u64>,
}

/// A `D014` Deployment's generation and revision at detection; either may be absent from what
/// kausa holds, and then there is no create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Deployment {
    pub generation: Option<i64>,
    pub revision: Option<u64>,
}

/// The object an Event incident is about, when it is not a Pod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Regarding {
    pub kind: String,
    pub name: String,
    pub uid: Option<String>,
}

/// What an Incident says: the `spec` of point 2, before its check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Spec {
    pub detection: Detection,
    pub fingerprint: String,
    pub first_seen: Timestamp,
    pub settled_at: Timestamp,
    pub object: Object,
    pub facts: Facts,
    pub sample: Option<Sample>,
    pub deployment: Option<Deployment>,
    pub regarding: Option<Regarding>,
    pub missing: Missing,
}

/// An Incident that passed the check: its name and its `spec`, as the create sends them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Checked {
    pub name: String,
    pub spec: Value,
}

/// `time` in UTC, truncated to the second: `YYYY-MM-DDTHH:MM:SSZ` for the years 0 to 9999.
pub(crate) fn time(time: Timestamp) -> String {
    time.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Whether `text` fits `TIME_PATTERN`.
pub(crate) fn is_time(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == TIME_LEN
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            10 => *b == b'T',
            13 | 16 => *b == b':',
            19 => *b == b'Z',
            _ => b.is_ascii_digit(),
        })
}

/// Whether `text` fits `FINGERPRINT_PATTERN`.
pub(crate) fn is_fingerprint(text: &str) -> bool {
    text.len() == FINGERPRINT_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `text` fits `KIND_PATTERN` within `KIND_LEN`.
pub(crate) fn is_kind(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() <= KIND_LEN
        && bytes.first().is_some_and(u8::is_ascii_uppercase)
        && bytes.iter().all(u8::is_ascii_alphanumeric)
}

/// Whether `text` fits `API_VERSION_PATTERN` within `MAX_NAME_LEN`.
pub(crate) fn is_api_version(text: &str) -> bool {
    let is_version = |version: &str| {
        let bytes = version.as_bytes();
        let edge = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(edge)
            && bytes.iter().all(|b| edge(b) || *b == b'-')
    };
    text.len() <= MAX_NAME_LEN
        && match text.split_once('/') {
            Some((group, version)) => is_dns_subdomain(group) && is_version(version),
            None => is_version(text),
        }
}

/// Whether `reason` is on the list of any detection: the schema's enum.
pub(crate) fn is_reason(reason: &str) -> bool {
    DETECTIONS
        .into_iter()
        .any(|detection| fact::reasons(detection).contains(&reason))
}

/// The detections whose incidents are about an object that is not a Pod: `podsSeen` is 0 exactly
/// for these.
pub(crate) fn sees_no_pod(detection: Detection) -> bool {
    matches!(
        detection,
        Detection::JobFailed
            | Detection::PodCreate
            | Detection::ClaimProvisioning
            | Detection::AutoscalerMetrics
            | Detection::RolloutStalled
    )
}

fn ensure(holds: bool, field: Field) -> Result<(), Field> {
    if holds { Ok(()) } else { Err(field) }
}

fn ensure_uid(uid: Option<&str>, field: Field) -> Result<(), Field> {
    ensure(uid.is_none_or(|uid| Uid::parse(uid).is_some()), field)
}

fn ensure_revision(revision: Option<u64>, field: Field) -> Result<(), Field> {
    ensure(
        revision.is_none_or(|r| (1..=MAX_REVISION).contains(&r)),
        field,
    )
}

impl Spec {
    /// The Incident's name, `kausa.<fingerprint>.<firstSeenUnix>`; `check` says whether it fits.
    pub(crate) fn name(&self) -> String {
        format!("kausa.{}.{}", self.fingerprint, self.first_seen.as_second())
    }

    /// Checks every field as the schema would, and the name as kurita's record reads it: the
    /// first field that does not fit, in the schema's order.
    pub(crate) fn check(&self) -> Result<(), Field> {
        ensure(is_fingerprint(&self.fingerprint), Field::Fingerprint)?;
        ensure(is_time(&time(self.first_seen)), Field::FirstSeen)?;
        // From the epoch, so the seconds have no sign and match `firstSeen`, and under 10^10, so
        // they have at most 10 digits.
        ensure(
            self.first_seen >= Timestamp::UNIX_EPOCH
                && self.first_seen.as_second() < NAME_SECONDS_END,
            Field::Name,
        )?;
        ensure(is_time(&time(self.settled_at)), Field::SettledAt)?;

        let object = &self.object;
        ensure(is_api_version(&object.api_version), Field::ObjectApiVersion)?;
        ensure(is_kind(&object.kind), Field::ObjectKind)?;
        ensure(is_dns_subdomain(&object.name), Field::ObjectName)?;
        ensure_uid(object.uid.as_deref(), Field::ObjectUid)?;

        let facts = &self.facts;
        ensure(facts.reason.is_none_or(is_reason), Field::FactsReason)?;
        ensure(
            facts.container.as_deref().is_none_or(is_dns_label),
            Field::FactsContainer,
        )?;
        ensure(
            facts.pods_seen <= MAX_PODS_SEEN
                && (facts.pods_seen == 0) == sees_no_pod(self.detection),
            Field::FactsPodsSeen,
        )?;
        ensure(
            facts.pods_seen_is_minimum == (facts.pods_seen == MAX_PODS_SEEN),
            Field::FactsPodsSeenIsMinimum,
        )?;
        ensure(facts.pods_now <= MAX_PODS_NOW, Field::FactsPodsNow)?;

        ensure(
            self.sample.is_some() == (facts.pods_seen > 0),
            Field::Sample,
        )?;
        if let Some(sample) = &self.sample {
            ensure(is_dns_subdomain(&sample.pod_name), Field::SamplePodName)?;
            ensure_uid(Some(&sample.pod_uid), Field::SamplePodUid)?;
            if let Some(controller) = &sample.controller {
                ensure_uid(Some(&controller.uid), Field::SampleControllerUid)?;
                ensure_revision(controller.revision, Field::SampleControllerRevision)?;
                ensure(
                    controller.revision.is_none() || controller.kind == ControllerKind::ReplicaSet,
                    Field::SampleControllerRevision,
                )?;
            }
        }

        ensure(
            self.deployment.is_some() == (self.detection == Detection::RolloutStalled),
            Field::Deployment,
        )?;
        if let Some(deployment) = self.deployment {
            ensure(
                deployment.generation.is_some_and(|g| g >= 1),
                Field::DeploymentGeneration,
            )?;
            ensure(deployment.revision.is_some(), Field::DeploymentRevision)?;
            ensure_revision(deployment.revision, Field::DeploymentRevision)?;
        }

        if let Some(regarding) = &self.regarding {
            ensure(
                REGARDING_KINDS.contains(&regarding.kind.as_str()),
                Field::RegardingKind,
            )?;
            ensure(is_dns_subdomain(&regarding.name), Field::RegardingName)?;
            ensure_uid(regarding.uid.as_deref(), Field::RegardingUid)?;
        }

        // An entry holds a `sample` or a `regarding`, never both (point 3).
        ensure(
            self.sample.is_none() || self.regarding.is_none(),
            Field::Regarding,
        )?;
        // `owner-lookup-failed` only where a lookup exists: the sample Pod's, or that of an Event's
        // Job or ReplicaSet, which then names `object` as `regarding` does (point 3).
        let event_lookup = matches!(self.detection, Detection::JobFailed | Detection::PodCreate);
        let owner_failed = self.missing.owner_lookup_failed;
        ensure(
            !owner_failed || self.sample.is_some() || event_lookup,
            Field::Missing,
        )?;
        ensure(
            !(owner_failed && event_lookup)
                || self.regarding.as_ref().is_some_and(|regarding| {
                    regarding.kind == object.kind
                        && regarding.name == object.name
                        && regarding.uid == object.uid
                }),
            Field::Object,
        )?;

        let read_revision_failed = self
            .sample
            .as_ref()
            .and_then(|sample| sample.controller.as_ref())
            .is_some_and(|c| c.kind == ControllerKind::ReplicaSet && c.revision.is_none());
        ensure(
            self.missing.revision_lookup_failed == read_revision_failed,
            Field::Missing,
        )
    }

    /// The name and the `spec` of the create, or the first field that does not fit.
    pub(crate) fn checked(&self) -> Result<Checked, Field> {
        self.check()?;
        Ok(Checked {
            name: self.name(),
            spec: self.to_json(),
        })
    }

    fn to_json(&self) -> Value {
        let mut spec = Map::new();
        spec.insert("detection".into(), json!(self.detection.id()));
        spec.insert("fingerprint".into(), json!(self.fingerprint));
        spec.insert("firstSeen".into(), json!(time(self.first_seen)));
        spec.insert("settledAt".into(), json!(time(self.settled_at)));
        let mut object = Map::new();
        object.insert("apiVersion".into(), json!(self.object.api_version));
        object.insert("kind".into(), json!(self.object.kind));
        object.insert("name".into(), json!(self.object.name));
        if let Some(uid) = &self.object.uid {
            object.insert("uid".into(), json!(uid));
        }
        spec.insert("object".into(), Value::Object(object));
        let mut facts = Map::new();
        if let Some(reason) = self.facts.reason {
            facts.insert("reason".into(), json!(reason));
        }
        if let Some(code) = self.facts.exit_code {
            facts.insert("exitCode".into(), json!(code));
        }
        if let Some(container) = &self.facts.container {
            facts.insert("container".into(), json!(container));
        }
        facts.insert("podsSeen".into(), json!(self.facts.pods_seen));
        facts.insert(
            "podsSeenIsMinimum".into(),
            json!(self.facts.pods_seen_is_minimum),
        );
        facts.insert("podsNow".into(), json!(self.facts.pods_now));
        spec.insert("facts".into(), Value::Object(facts));
        if let Some(sample) = &self.sample {
            let mut value = Map::new();
            value.insert(
                "pod".into(),
                json!({"name": sample.pod_name, "uid": sample.pod_uid}),
            );
            if let Some(controller) = &sample.controller {
                let mut c = Map::new();
                c.insert("kind".into(), json!(controller.kind.as_str()));
                c.insert("uid".into(), json!(controller.uid));
                if let Some(revision) = controller.revision {
                    c.insert("revision".into(), json!(revision));
                }
                value.insert("controller".into(), Value::Object(c));
            }
            spec.insert("sample".into(), Value::Object(value));
        }
        if let Some(deployment) = self.deployment {
            spec.insert(
                "deployment".into(),
                json!({"generation": deployment.generation, "revision": deployment.revision}),
            );
        }
        if let Some(regarding) = &self.regarding {
            let mut value = Map::new();
            value.insert("kind".into(), json!(regarding.kind));
            value.insert("name".into(), json!(regarding.name));
            if let Some(uid) = &regarding.uid {
                value.insert("uid".into(), json!(uid));
            }
            spec.insert("regarding".into(), Value::Object(value));
        }
        spec.insert("missing".into(), json!(self.missing.values()));
        Value::Object(spec)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use regex::Regex;

    use super::*;

    const UID_POD: &str = "00000000-0000-4000-8000-000000000001";
    const UID_RS: &str = "00000000-0000-4000-8000-000000000002";
    const UID_OWNER: &str = "00000000-0000-4000-8000-000000000003";
    const FP: &str = "5d0c2b7e9a1f4c38";

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a timestamp")
    }

    fn seconds(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("in jiff's range")
    }

    /// A crash loop of a Deployment's Pods, whose ReplicaSet's revision was read.
    pub(super) fn pod_spec() -> Spec {
        Spec {
            detection: Detection::CrashLoop,
            fingerprint: FP.into(),
            first_seen: at("2026-11-24T10:00:00.750Z"),
            settled_at: at("2026-11-24T10:05:00Z"),
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

    /// A ReplicaSet that cannot create its Pods, from an Event about it.
    fn event_spec() -> Spec {
        Spec {
            detection: Detection::PodCreate,
            facts: Facts {
                reason: Some("FailedCreate"),
                exit_code: None,
                container: None,
                pods_seen: 0,
                pods_seen_is_minimum: false,
                pods_now: 0,
            },
            sample: None,
            regarding: Some(Regarding {
                kind: "ReplicaSet".into(),
                name: "web-7d4b9c6f5d".into(),
                uid: Some(UID_RS.into()),
            }),
            ..pod_spec()
        }
    }

    /// A rollout that does not progress.
    fn rollout_spec() -> Spec {
        Spec {
            detection: Detection::RolloutStalled,
            facts: Facts {
                reason: Some("ProgressDeadlineExceeded"),
                exit_code: None,
                container: None,
                pods_seen: 0,
                pods_seen_is_minimum: false,
                pods_now: 2,
            },
            sample: None,
            deployment: Some(Deployment {
                generation: Some(12),
                revision: Some(7),
            }),
            ..pod_spec()
        }
    }

    /// Every value at its longest: names, kinds and UIDs at their bounds, the longest reason, the
    /// lowest exit code, a capped count and the highest revision.
    fn longest_spec() -> Spec {
        Spec {
            detection: Detection::AutoscalerMetrics,
            first_seen: seconds(NAME_SECONDS_END - 1),
            settled_at: seconds(NAME_SECONDS_END - 1),
            object: Object {
                api_version: format!("{}/{}", "g".repeat(189), "v".repeat(63)),
                kind: "K".repeat(KIND_LEN),
                name: "n".repeat(MAX_NAME_LEN),
                uid: Some(UID_OWNER.into()),
            },
            facts: Facts {
                reason: Some("FailedGetContainerResourceMetric"),
                exit_code: Some(i32::MIN),
                container: Some("c".repeat(MAX_NAMESPACE_LEN)),
                pods_seen: 0,
                pods_seen_is_minimum: false,
                pods_now: MAX_PODS_NOW,
            },
            sample: None,
            regarding: Some(Regarding {
                kind: "HorizontalPodAutoscaler".into(),
                name: "r".repeat(MAX_NAME_LEN),
                uid: Some(UID_RS.into()),
            }),
            ..pod_spec()
        }
    }

    /// The longest spec of an incident about Pods: a sample with a ReplicaSet and its revision.
    fn longest_pod_spec() -> Spec {
        Spec {
            detection: Detection::ContainerCreate,
            facts: Facts {
                reason: Some("CreateContainerConfigError"),
                pods_seen: MAX_PODS_SEEN,
                pods_seen_is_minimum: true,
                ..longest_spec().facts
            },
            sample: Some(Sample {
                pod_name: "p".repeat(MAX_NAME_LEN),
                pod_uid: UID_POD.into(),
                controller: Some(Controller {
                    kind: ControllerKind::ReplicaSet,
                    uid: UID_RS.into(),
                    revision: Some(MAX_REVISION),
                }),
            }),
            regarding: None,
            missing: Missing {
                owner_lookup_failed: true,
                revision_lookup_failed: false,
            },
            ..longest_spec()
        }
    }

    // --- The schema file, shared with the chart -----------------------------------------------

    fn schema() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/charts/kausa/files/incident-schema.json"
        );
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path} is not JSON: {e}"))
    }

    /// The schema of the field at `path` under `spec`, such as `sample.pod.uid`.
    fn field(path: &str) -> Value {
        let mut node = schema()["properties"]["spec"].clone();
        for key in path.split('.') {
            node = node["properties"][key].clone();
            assert!(node.is_object(), "{path}: no {key} in the schema");
        }
        node
    }

    fn keys(value: &Value) -> BTreeSet<String> {
        value
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn strings(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap_or_else(|| panic!("{value} is not an array"))
            .iter()
            .map(|v| v.as_str().expect("a string").to_owned())
            .collect()
    }

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    fn rules(schema: &Value) -> Vec<(String, String)> {
        schema["x-kubernetes-validations"]
            .as_array()
            .map(|r| {
                r.iter()
                    .map(|v| {
                        (
                            v["rule"].as_str().expect("a rule").to_owned(),
                            v["message"].as_str().expect("a message").to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Walks every node of the schema, with its path.
    fn walk(schema: &Value, path: &str, visit: &mut dyn FnMut(&Value, &str)) {
        visit(schema, path);
        if let Some(properties) = schema["properties"].as_object() {
            for (key, value) in properties {
                walk(value, &format!("{path}.{key}"), visit);
            }
        }
        if schema.get("items").is_some() {
            walk(&schema["items"], &format!("{path}[]"), visit);
        }
    }

    #[test]
    fn the_schema_requires_an_immutable_spec_and_keeps_status_empty() {
        let s = schema();
        assert_eq!(s["type"], "object");
        assert_eq!(strings(&s["required"]), ["spec"]);
        assert_eq!(keys(&s["properties"]), set(&["spec", "status"]));
        let status = &s["properties"]["status"];
        assert_eq!(status["type"], "object");
        assert_eq!(keys(status), set(&["type", "description"]));
        let spec = &s["properties"]["spec"];
        assert_eq!(
            rules(spec)[0],
            (RULE_IMMUTABLE.into(), "spec is immutable".into())
        );
        assert_eq!(
            keys(&spec["properties"]),
            set(&[
                "detection",
                "fingerprint",
                "firstSeen",
                "settledAt",
                "object",
                "facts",
                "sample",
                "deployment",
                "regarding",
                "missing",
            ])
        );
        let required = |path: &str| {
            let node = if path.is_empty() {
                spec.clone()
            } else {
                field(path)
            };
            strings(&node["required"])
        };
        assert_eq!(
            required(""),
            [
                "detection",
                "fingerprint",
                "firstSeen",
                "settledAt",
                "object",
                "facts",
                "missing",
            ]
        );
        assert_eq!(required("object"), ["apiVersion", "kind", "name"]);
        assert_eq!(
            required("facts"),
            ["podsSeen", "podsSeenIsMinimum", "podsNow"]
        );
        assert_eq!(required("sample"), ["pod"]);
        assert_eq!(required("sample.pod"), ["name", "uid"]);
        assert_eq!(required("sample.controller"), ["kind", "uid"]);
        assert_eq!(required("deployment"), ["generation", "revision"]);
        assert_eq!(required("regarding"), ["kind", "name"]);
    }

    #[test]
    fn every_node_is_typed_and_described_and_every_string_is_capped() {
        walk(&schema(), "", &mut |node, path| {
            assert!(node["type"].is_string(), "{path} has no type");
            if !path.is_empty() {
                assert!(
                    node["description"]
                        .as_str()
                        .is_some_and(|d| !d.trim().is_empty()),
                    "{path} has no description"
                );
            }
            assert!(
                node.get("x-kubernetes-preserve-unknown-fields").is_none()
                    && node.get("additionalProperties").is_none(),
                "{path} keeps unknown fields"
            );
            if node["type"] == "string" {
                assert!(
                    node["enum"].is_array()
                        || (node["pattern"].is_string() && node["maxLength"].is_u64()),
                    "{path} is neither an enum nor a pattern with a maxLength"
                );
            }
            if node["type"] == "array" {
                assert!(node["maxItems"].is_u64(), "{path} has no maxItems");
            }
        });
    }

    #[test]
    fn detections_lists_every_variant_of_detection() {
        // A new variant breaks this match, a reminder to add it to `DETECTIONS` too.
        for detection in DETECTIONS {
            match detection {
                Detection::CrashLoop
                | Detection::ImagePull
                | Detection::OutOfMemory
                | Detection::ContainerCreate
                | Detection::Unschedulable
                | Detection::ProbeFailure
                | Detection::VolumeFailure
                | Detection::JobFailed
                | Detection::PodEvicted
                | Detection::SandboxCreate
                | Detection::PodCreate
                | Detection::ClaimProvisioning
                | Detection::AutoscalerMetrics
                | Detection::RolloutStalled => {}
            }
        }
        // Each variant once, in the order of the enum, up to its last.
        let discriminants: Vec<usize> = DETECTIONS.iter().map(|&d| d as usize).collect();
        assert_eq!(
            discriminants,
            (0..=Detection::RolloutStalled as usize).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_enums_equal_kausa_s_vocabularies() {
        let detections: Vec<String> = DETECTIONS.iter().map(|d| d.id().to_owned()).collect();
        let ids: Vec<String> = (1..=14).map(|i| format!("D{i:03}")).collect();
        assert_eq!(
            detections, ids,
            "DETECTIONS holds every detection once, in order"
        );
        assert_eq!(strings(&field("detection")["enum"]), detections);
        let mut reasons: Vec<String> = Vec::new();
        for detection in DETECTIONS {
            for reason in fact::reasons(detection) {
                if !reasons.iter().any(|r| r == reason) {
                    reasons.push((*reason).to_owned());
                }
            }
        }
        assert_eq!(strings(&field("facts.reason")["enum"]), reasons);
        assert!(reasons.iter().all(|r| is_reason(r)));
        assert!(!is_reason("Completed") && !is_reason(""));
        let kinds: Vec<&str> = ControllerKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(strings(&field("sample.controller.kind")["enum"]), kinds);
        assert_eq!(strings(&field("regarding.kind")["enum"]), REGARDING_KINDS);
        let missing = schema()["properties"]["spec"]["properties"]["missing"].clone();
        assert_eq!(strings(&missing["items"]["enum"]), MISSING_VALUES);
        assert_eq!(
            missing["maxItems"].as_u64(),
            Some(MISSING_VALUES.len() as u64)
        );
        assert_eq!(missing["x-kubernetes-list-type"], "set");
    }

    /// Each string field's pattern and maxLength, as the checker's constants give them.
    fn string_fields() -> Vec<(&'static str, &'static str, usize)> {
        vec![
            ("fingerprint", FINGERPRINT_PATTERN, FINGERPRINT_LEN),
            ("firstSeen", TIME_PATTERN, TIME_LEN),
            ("settledAt", TIME_PATTERN, TIME_LEN),
            ("object.apiVersion", API_VERSION_PATTERN, MAX_NAME_LEN),
            ("object.kind", KIND_PATTERN, KIND_LEN),
            ("object.name", NAME_PATTERN, MAX_NAME_LEN),
            ("object.uid", UID_PATTERN, UID_LEN),
            ("facts.container", LABEL_PATTERN, MAX_NAMESPACE_LEN),
            ("sample.pod.name", NAME_PATTERN, MAX_NAME_LEN),
            ("sample.pod.uid", UID_PATTERN, UID_LEN),
            ("sample.controller.uid", UID_PATTERN, UID_LEN),
            ("regarding.name", NAME_PATTERN, MAX_NAME_LEN),
            ("regarding.uid", UID_PATTERN, UID_LEN),
        ]
    }

    #[test]
    fn the_patterns_and_lengths_equal_the_checker_s_constants() {
        let mut seen = BTreeSet::new();
        for (path, pattern, length) in string_fields() {
            let node = field(path);
            assert_eq!(node["pattern"], pattern, "{path}");
            assert_eq!(node["maxLength"].as_u64(), Some(length as u64), "{path}");
            seen.insert(path);
        }
        // Every string field with a pattern is listed above.
        walk(&schema()["properties"]["spec"], "", &mut |node, path| {
            if node["pattern"].is_string() {
                assert!(seen.contains(&path[1..]), "{path} is not listed");
            }
        });
        assert_eq!(MAX_NAME_LEN, 253);
        assert_eq!(MAX_NAMESPACE_LEN, 63);
    }

    /// Whether `text` fits the schema's string: its pattern, as Kubernetes' RE2 reads it, and its
    /// maxLength in characters.
    fn fits(pattern: &Regex, max: usize, text: &str) -> bool {
        pattern.is_match(text) && text.chars().count() <= max
    }

    #[test]
    fn each_pattern_agrees_with_its_checker() {
        type Checker = fn(&str) -> bool;
        let uid: Checker = |t| Uid::parse(t).is_some();
        let checkers: [(&str, usize, Checker, Vec<String>); 7] = [
            (
                FINGERPRINT_PATTERN,
                FINGERPRINT_LEN,
                is_fingerprint,
                vec![FP.into()],
            ),
            (
                TIME_PATTERN,
                TIME_LEN,
                is_time,
                vec!["2026-11-24T10:00:00Z".into()],
            ),
            (UID_PATTERN, UID_LEN, uid, vec![UID_POD.into()]),
            (
                NAME_PATTERN,
                MAX_NAME_LEN,
                is_dns_subdomain,
                vec![
                    "a".into(),
                    "web-0.a-b.c".into(),
                    "a".repeat(100),
                    "a".repeat(MAX_NAME_LEN),
                    format!(
                        "{}{}",
                        format!("{}.", "a".repeat(63)).repeat(3),
                        "a".repeat(61)
                    ),
                ],
            ),
            (
                LABEL_PATTERN,
                MAX_NAMESPACE_LEN,
                is_dns_label,
                vec!["a".into(), "app-1".into(), "a".repeat(MAX_NAMESPACE_LEN)],
            ),
            (
                KIND_PATTERN,
                KIND_LEN,
                is_kind,
                vec!["A1".into(), "Deployment".into(), "K".repeat(KIND_LEN)],
            ),
            (
                API_VERSION_PATTERN,
                MAX_NAME_LEN,
                is_api_version,
                vec![
                    "v1".into(),
                    "apps/v1".into(),
                    "a.b-c/v1beta1".into(),
                    "v".repeat(63),
                    format!("{}/{}", "g".repeat(189), "v".repeat(63)),
                    format!("{}/v1", "g".repeat(250)),
                    "v".repeat(64),
                    "v".repeat(MAX_NAME_LEN),
                    format!("g/{}", "v".repeat(MAX_NAME_LEN - 2)),
                ],
            ),
        ];
        let alphabet: Vec<char> = "az09fg-./AZ_ :Té\u{663}\n".chars().collect();
        for (pattern, max, checker, bases) in checkers {
            let regex = Regex::new(pattern).expect("the pattern compiles");
            let mut corpus: Vec<String> = vec![String::new()];
            for a in &alphabet {
                corpus.push(a.to_string());
                for b in &alphabet {
                    corpus.push(format!("{a}{b}"));
                    for c in &alphabet {
                        corpus.push(format!("{a}{b}{c}"));
                    }
                }
            }
            for base in &bases {
                assert!(checker(base), "{pattern}: {base} is a valid example");
                let chars: Vec<char> = base.chars().collect();
                for i in 0..=chars.len() {
                    for a in &alphabet {
                        let mut replaced = chars.clone();
                        if i < chars.len() {
                            replaced[i] = *a;
                            corpus.push(replaced.iter().collect());
                        }
                        let mut inserted = chars.clone();
                        inserted.insert(i, *a);
                        corpus.push(inserted.iter().collect());
                    }
                    if i < chars.len() {
                        let mut removed = chars.clone();
                        removed.remove(i);
                        corpus.push(removed.iter().collect());
                    }
                }
            }
            for text in &corpus {
                assert_eq!(
                    checker(text),
                    fits(&regex, max, text),
                    "{pattern} and its checker differ on {text:?}"
                );
            }
        }
    }

    #[test]
    fn an_api_version_is_read_as_kurita_reads_one() {
        // kurita's `object.apiVersion`, byte for byte.
        assert_eq!(
            API_VERSION_PATTERN,
            concat!(
                r"^([a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*/)?",
                r"[a-z]([-a-z0-9]*[a-z0-9])?$"
            )
        );
        let regex = Regex::new(API_VERSION_PATTERN).expect("the pattern compiles");
        // A version starts with a letter, and has no limit of its own inside 253.
        for (text, valid) in [
            ("1", false),
            ("1v", false),
            ("apps/1", false),
            ("apps/1beta1", false),
            ("a.b/v", true),
            (&"v".repeat(64), true),
            (&format!("apps/{}", "v".repeat(100)), true),
            (&"v".repeat(MAX_NAME_LEN), true),
            (&"v".repeat(MAX_NAME_LEN + 1), false),
        ] {
            assert_eq!(is_api_version(text), valid, "{text}");
            assert_eq!(fits(&regex, MAX_NAME_LEN, text), valid, "{text}");
        }
    }

    #[test]
    fn the_integer_ranges_equal_the_checks() {
        let range = |path: &str| {
            let node = field(path);
            assert_eq!(node["type"], "integer", "{path}");
            (node["minimum"].as_i64(), node["maximum"].as_u64())
        };
        assert_eq!(
            range("facts.exitCode"),
            (Some(i32::MIN.into()), Some(i32::MAX as u64))
        );
        assert_eq!(field("facts.exitCode")["format"], "int32");
        assert_eq!(
            range("facts.podsSeen"),
            (Some(0), Some(MAX_PODS_SEEN as u64))
        );
        assert_eq!(range("facts.podsNow"), (Some(0), Some(MAX_PODS_NOW as u64)));
        assert_eq!(
            range("sample.controller.revision"),
            (Some(1), Some(MAX_REVISION))
        );
        assert_eq!(range("deployment.revision"), (Some(1), Some(MAX_REVISION)));
        assert_eq!(range("deployment.generation"), (Some(1), None));
        for path in [
            "sample.controller.revision",
            "deployment.revision",
            "deployment.generation",
        ] {
            assert_eq!(field(path)["format"], "int64", "{path}");
        }
        assert_eq!(field("facts.podsSeenIsMinimum")["type"], "boolean");
        assert_eq!(MAX_REVISION, 9_007_199_254_740_992);
        assert_eq!(MAX_PODS_SEEN, 100);
    }

    #[test]
    fn the_cel_rules_are_pinned() {
        let spec = schema()["properties"]["spec"].clone();
        let texts = |node: &Value| rules(node).into_iter().map(|(r, _)| r).collect::<Vec<_>>();
        assert_eq!(
            texts(&spec),
            [
                RULE_IMMUTABLE,
                RULE_SAMPLE,
                RULE_PODS_SEEN,
                RULE_DEPLOYMENT,
                RULE_MISSING_REVISION,
                RULE_SAMPLE_OR_REGARDING,
                RULE_MISSING_OWNER,
                RULE_OWNER_LOOKUP_OBJECT,
            ]
        );
        assert_eq!(texts(&field("facts")), [RULE_MINIMUM]);
        assert_eq!(
            texts(&field("sample.controller")),
            [RULE_CONTROLLER_REVISION]
        );
        // No other node has a rule.
        let mut with_rules = Vec::new();
        walk(&schema(), "", &mut |node, path| {
            if node.get("x-kubernetes-validations").is_some() {
                with_rules.push(path.to_owned());
            }
        });
        assert_eq!(
            with_rules,
            [".spec", ".spec.facts", ".spec.sample.controller"]
        );
        // The ids in RULE_PODS_SEEN are those of `sees_no_pod`, and 100 is `MAX_PODS_SEEN`.
        let no_pod: Vec<String> = DETECTIONS
            .into_iter()
            .filter(|d| sees_no_pod(*d))
            .map(|d| format!("'{}'", d.id()))
            .collect();
        assert!(RULE_PODS_SEEN.contains(&format!("[{}]", no_pod.join(", "))));
        assert!(RULE_MINIMUM.contains(&format!("== {MAX_PODS_SEEN})")));
    }

    #[test]
    fn the_sample_rule_holds_both_ways() {
        // podsSeen above 0 with a sample, and 0 without one: accepted.
        assert_eq!(pod_spec().check(), Ok(()));
        assert_eq!(event_spec().check(), Ok(()));
        // Above 0 without a sample, or 0 with one: refused.
        let mut spec = pod_spec();
        spec.sample = None;
        spec.missing = Missing::default();
        assert_eq!(spec.check(), Err(Field::Sample));
        let mut spec = event_spec();
        spec.sample = pod_spec().sample;
        assert_eq!(spec.check(), Err(Field::Sample));
    }

    #[test]
    fn the_pods_seen_rule_holds_both_ways() {
        for detection in DETECTIONS {
            let mut spec = if sees_no_pod(detection) {
                event_spec()
            } else {
                pod_spec()
            };
            spec.detection = detection;
            spec.deployment = rollout_spec()
                .deployment
                .filter(|_| detection == Detection::RolloutStalled);
            spec.facts.reason = None;
            assert_eq!(spec.check(), Ok(()), "{detection:?}");
            // The other count: 0 for a detection about Pods, 1 for one about another object.
            spec.facts.pods_seen = usize::from(sees_no_pod(detection));
            assert_eq!(spec.check(), Err(Field::FactsPodsSeen), "{detection:?}");
        }
        let mut spec = pod_spec();
        spec.facts.pods_seen = MAX_PODS_SEEN + 1;
        spec.facts.pods_seen_is_minimum = true;
        assert_eq!(spec.check(), Err(Field::FactsPodsSeen));
    }

    #[test]
    fn the_minimum_rule_holds_both_ways() {
        let mut spec = pod_spec();
        spec.facts.pods_seen = MAX_PODS_SEEN;
        spec.facts.pods_seen_is_minimum = true;
        assert_eq!(spec.check(), Ok(()));
        spec.facts.pods_seen_is_minimum = false;
        assert_eq!(spec.check(), Err(Field::FactsPodsSeenIsMinimum));
        spec.facts.pods_seen = MAX_PODS_SEEN - 1;
        assert_eq!(spec.check(), Ok(()));
        spec.facts.pods_seen_is_minimum = true;
        assert_eq!(spec.check(), Err(Field::FactsPodsSeenIsMinimum));
        // At 0 the flag is false.
        let mut spec = event_spec();
        spec.facts.pods_seen_is_minimum = true;
        assert_eq!(spec.check(), Err(Field::FactsPodsSeenIsMinimum));
    }

    #[test]
    fn the_deployment_rule_holds_both_ways() {
        assert_eq!(rollout_spec().check(), Ok(()));
        let mut spec = rollout_spec();
        spec.deployment = None;
        assert_eq!(spec.check(), Err(Field::Deployment));
        let mut spec = pod_spec();
        spec.deployment = rollout_spec().deployment;
        assert_eq!(spec.check(), Err(Field::Deployment));
        let mut spec = event_spec();
        spec.deployment = rollout_spec().deployment;
        assert_eq!(spec.check(), Err(Field::Deployment));
    }

    #[test]
    fn a_d014_with_no_generation_or_revision_is_refused() {
        for (generation, revision, refusal) in [
            (None, Some(7), Field::DeploymentGeneration),
            (Some(0), Some(7), Field::DeploymentGeneration),
            (Some(-1), Some(7), Field::DeploymentGeneration),
            (Some(1), None, Field::DeploymentRevision),
            (Some(1), Some(0), Field::DeploymentRevision),
            (Some(1), Some(MAX_REVISION + 1), Field::DeploymentRevision),
        ] {
            let mut spec = rollout_spec();
            spec.deployment = Some(Deployment {
                generation,
                revision,
            });
            assert_eq!(spec.check(), Err(refusal), "{generation:?} {revision:?}");
        }
        let mut spec = rollout_spec();
        spec.deployment = Some(Deployment {
            generation: Some(i64::MAX),
            revision: Some(MAX_REVISION),
        });
        assert_eq!(spec.check(), Ok(()));
    }

    #[test]
    fn the_missing_revision_rule_holds_both_ways() {
        let without_revision = |kind: ControllerKind, flag: bool| {
            let mut spec = pod_spec();
            if let Some(controller) = spec.sample.as_mut().and_then(|s| s.controller.as_mut()) {
                controller.kind = kind;
                controller.revision = None;
            }
            spec.missing.revision_lookup_failed = flag;
            spec.check()
        };
        // A ReplicaSet with no revision, and the flag: accepted; without the flag: refused.
        assert_eq!(without_revision(ControllerKind::ReplicaSet, true), Ok(()));
        assert_eq!(
            without_revision(ControllerKind::ReplicaSet, false),
            Err(Field::Missing)
        );
        // Any other controller has no revision and no flag.
        for kind in ControllerKind::ALL.into_iter().skip(1) {
            assert_eq!(without_revision(kind, false), Ok(()), "{kind:?}");
            assert_eq!(
                without_revision(kind, true),
                Err(Field::Missing),
                "{kind:?}"
            );
        }
        // A ReplicaSet with its revision takes no flag.
        let mut spec = pod_spec();
        spec.missing.revision_lookup_failed = true;
        assert_eq!(spec.check(), Err(Field::Missing));
        // No controller, or no sample: no flag.
        let mut spec = pod_spec();
        spec.sample.as_mut().expect("a sample").controller = None;
        assert_eq!(spec.check(), Ok(()));
        spec.missing.revision_lookup_failed = true;
        assert_eq!(spec.check(), Err(Field::Missing));
        let mut spec = event_spec();
        spec.missing.revision_lookup_failed = true;
        assert_eq!(spec.check(), Err(Field::Missing));
        // The owner's flag is the other rules' (`the_owner_lookup_rules_hold_both_ways`).
        let mut spec = pod_spec();
        spec.missing.owner_lookup_failed = true;
        assert_eq!(spec.check(), Ok(()));
    }

    /// `event_spec` as kausa writes it when the lookup of its ReplicaSet failed: `object` names
    /// the ReplicaSet that `regarding` names.
    fn unresolved_event_spec() -> Spec {
        let mut spec = event_spec();
        spec.object = Object {
            api_version: "apps/v1".into(),
            kind: "ReplicaSet".into(),
            name: "web-7d4b9c6f5d".into(),
            uid: Some(UID_RS.into()),
        };
        spec.missing.owner_lookup_failed = true;
        spec
    }

    #[test]
    fn the_owner_lookup_rules_hold_both_ways() {
        // Never both a sample and a regarding.
        let mut spec = pod_spec();
        spec.regarding = event_spec().regarding;
        assert_eq!(spec.check(), Err(Field::Regarding));
        // The flag with a sample Pod, or for D008 and D011 naming the Job or ReplicaSet itself.
        assert_eq!(unresolved_event_spec().check(), Ok(()));
        let mut job = unresolved_event_spec();
        job.detection = Detection::JobFailed;
        job.facts.reason = Some("BackoffLimitExceeded");
        job.object.api_version = "batch/v1".into();
        for spec in [
            &mut job.object.kind,
            &mut job.regarding.as_mut().expect("r").kind,
        ] {
            *spec = "Job".into();
        }
        assert_eq!(job.check(), Ok(()));
        let mut no_uids = unresolved_event_spec();
        no_uids.object.uid = None;
        no_uids.regarding.as_mut().expect("a regarding").uid = None;
        assert_eq!(no_uids.check(), Ok(()));
        // Never for D012, D013 or D014, where no lookup exists.
        for detection in [
            Detection::ClaimProvisioning,
            Detection::AutoscalerMetrics,
            Detection::RolloutStalled,
        ] {
            let mut spec = if detection == Detection::RolloutStalled {
                rollout_spec()
            } else {
                Spec {
                    detection,
                    ..event_spec()
                }
            };
            spec.facts.reason = None;
            assert_eq!(spec.check(), Ok(()), "{detection:?}");
            spec.missing.owner_lookup_failed = true;
            assert_eq!(spec.check(), Err(Field::Missing), "{detection:?}");
        }
        // For D008 and D011, `object` is `regarding`'s object: same kind, name and UID.
        let changes: [fn(&mut Spec); 7] = [
            |s| s.object = pod_spec().object,
            |s| s.object.name = "web".into(),
            |s| s.object.kind = "Deployment".into(),
            |s| s.object.uid = Some(UID_OWNER.into()),
            |s| s.object.uid = None,
            |s| s.regarding.as_mut().expect("r").uid = None,
            |s| s.regarding = None,
        ];
        for (i, change) in changes.iter().enumerate() {
            let mut spec = unresolved_event_spec();
            change(&mut spec);
            assert_eq!(spec.check(), Err(Field::Object), "{i}");
            // Without the flag the owner is free.
            spec.missing.owner_lookup_failed = false;
            assert_eq!(spec.check(), Ok(()), "{i}");
        }
    }

    #[test]
    fn the_controller_revision_rule_holds_both_ways() {
        for kind in ControllerKind::ALL {
            let mut spec = pod_spec();
            spec.sample
                .as_mut()
                .and_then(|s| s.controller.as_mut())
                .expect("a controller")
                .kind = kind;
            let expected = if kind == ControllerKind::ReplicaSet {
                Ok(())
            } else {
                Err(Field::SampleControllerRevision)
            };
            assert_eq!(spec.check(), expected, "{kind:?}");
        }
    }

    /// A spec with one value that does not fit, for each field a refusal can name.
    fn refusals() -> Vec<(Field, Spec)> {
        let pod = pod_spec;
        fn controller(spec: &mut Spec) -> &mut Controller {
            spec.sample
                .as_mut()
                .and_then(|s| s.controller.as_mut())
                .expect("a controller")
        }
        let mut cases = Vec::new();
        let mut case = |field: Field, spec: Spec, change: &dyn Fn(&mut Spec)| {
            let mut spec = spec;
            change(&mut spec);
            cases.push((field, spec));
        };
        case(Field::Name, pod(), &|s| {
            s.first_seen = seconds(NAME_SECONDS_END)
        });
        case(Field::Name, pod(), &|s| {
            s.first_seen = at("1969-12-31T23:59:59.5Z")
        });
        case(Field::Fingerprint, pod(), &|s| {
            s.fingerprint = FP.to_uppercase()
        });
        case(Field::Fingerprint, pod(), &|s| {
            s.fingerprint = format!("{FP}0")
        });
        case(Field::FirstSeen, pod(), &|s| s.first_seen = Timestamp::MIN);
        case(Field::SettledAt, pod(), &|s| s.settled_at = Timestamp::MIN);
        case(Field::ObjectApiVersion, pod(), &|s| {
            s.object.api_version = "apps/v1/x".into()
        });
        case(Field::ObjectApiVersion, pod(), &|s| {
            s.object.api_version = "Apps/v1".into()
        });
        case(Field::ObjectKind, pod(), &|s| {
            s.object.kind = "deployment".into()
        });
        case(Field::ObjectKind, pod(), &|s| {
            s.object.kind = "Deploy\nment".into()
        });
        case(Field::ObjectName, pod(), &|s| {
            s.object.name = "web\n".into()
        });
        case(Field::ObjectName, pod(), &|s| {
            s.object.name = "a".repeat(MAX_NAME_LEN + 1)
        });
        case(Field::ObjectUid, pod(), &|s| {
            s.object.uid = Some(UID_OWNER.replace('4', "A"))
        });
        case(Field::FactsReason, pod(), &|s| {
            s.facts.reason = Some("Completed")
        });
        case(Field::FactsContainer, pod(), &|s| {
            s.facts.container = Some("App".into())
        });
        case(Field::FactsPodsSeen, pod(), &|s| s.facts.pods_seen = 0);
        case(Field::FactsPodsSeenIsMinimum, pod(), &|s| {
            s.facts.pods_seen_is_minimum = true
        });
        case(Field::FactsPodsNow, pod(), &|s| {
            s.facts.pods_now = MAX_PODS_NOW + 1
        });
        case(Field::Sample, pod(), &|s| s.sample = None);
        case(Field::SamplePodName, pod(), &|s| {
            s.sample.as_mut().expect("a sample").pod_name = "../x".into();
        });
        case(Field::SamplePodUid, pod(), &|s| {
            s.sample.as_mut().expect("a sample").pod_uid = String::new();
        });
        case(Field::SampleControllerUid, pod(), &|s| {
            controller(s).uid = "x".into()
        });
        case(Field::SampleControllerRevision, pod(), &|s| {
            controller(s).revision = Some(0)
        });
        case(Field::SampleControllerRevision, pod(), &|s| {
            controller(s).revision = Some(MAX_REVISION + 1);
        });
        case(Field::Deployment, rollout_spec(), &|s| s.deployment = None);
        case(Field::DeploymentGeneration, rollout_spec(), &|s| {
            s.deployment = Some(Deployment {
                generation: None,
                revision: Some(1),
            });
        });
        case(Field::DeploymentRevision, rollout_spec(), &|s| {
            s.deployment = Some(Deployment {
                generation: Some(1),
                revision: Some(MAX_REVISION + 1),
            });
        });
        case(Field::RegardingKind, event_spec(), &|s| {
            s.regarding.as_mut().expect("a regarding").kind = "Pod".into();
        });
        case(Field::RegardingName, event_spec(), &|s| {
            s.regarding.as_mut().expect("a regarding").name = "a/b".into();
        });
        case(Field::RegardingUid, event_spec(), &|s| {
            s.regarding.as_mut().expect("a regarding").uid = Some(format!("{UID_RS} "));
        });
        case(Field::Missing, pod(), &|s| {
            s.missing.revision_lookup_failed = true
        });
        case(Field::Missing, rollout_spec(), &|s| {
            s.missing.owner_lookup_failed = true
        });
        case(Field::Regarding, pod(), &|s| {
            s.regarding = event_spec().regarding
        });
        case(Field::Object, event_spec(), &|s| {
            s.missing.owner_lookup_failed = true
        });
        cases
    }

    #[test]
    fn each_field_has_its_refusal_and_it_names_no_value() {
        let mut refused = BTreeSet::new();
        for (field, spec) in refusals() {
            assert_eq!(spec.check(), Err(field), "{field:?}");
            assert_eq!(spec.checked(), Err(field), "{field:?}");
            refused.insert(field);
        }
        assert_eq!(refused, Field::ALL.into_iter().collect::<BTreeSet<_>>());
        // Each refusal names a path of the object: the name, or a field of the schema's spec.
        for refusal in Field::ALL {
            match refusal {
                Field::Name => assert_eq!(refusal.as_str(), "metadata.name"),
                _ => assert!(field(refusal.as_str()).is_object()),
            }
        }
        let paths: BTreeSet<&str> = Field::ALL.into_iter().map(Field::as_str).collect();
        assert_eq!(paths.len(), Field::ALL.len());
    }

    /// Checks `value` against `schema`: only listed properties, the required ones present, types,
    /// enums, patterns, lengths in characters, ranges and sets.
    fn conforms(value: &Value, schema: &Value, path: &str) {
        match schema["type"].as_str() {
            Some("object") => {
                let object = value
                    .as_object()
                    .unwrap_or_else(|| panic!("{path}: not an object"));
                for (key, v) in object {
                    let sub = &schema["properties"][key];
                    assert!(sub.is_object(), "{path}.{key} is not in the schema");
                    conforms(v, sub, &format!("{path}.{key}"));
                }
                if schema.get("required").is_some() {
                    for key in strings(&schema["required"]) {
                        assert!(object.contains_key(&key), "{path}.{key} is required");
                    }
                }
            }
            Some("array") => {
                let items = value
                    .as_array()
                    .unwrap_or_else(|| panic!("{path}: not an array"));
                let max = schema["maxItems"].as_u64().expect("a maxItems");
                assert!(items.len() as u64 <= max, "{path}: more than {max} items");
                let distinct: BTreeSet<String> = items.iter().map(Value::to_string).collect();
                assert_eq!(distinct.len(), items.len(), "{path}: not a set");
                for (i, item) in items.iter().enumerate() {
                    conforms(item, &schema["items"], &format!("{path}[{i}]"));
                }
            }
            Some("string") => {
                let text = value
                    .as_str()
                    .unwrap_or_else(|| panic!("{path}: not a string"));
                if let Some(allowed) = schema["enum"].as_array() {
                    assert!(allowed.contains(value), "{path}: {value} is not allowed");
                } else {
                    let pattern = Regex::new(schema["pattern"].as_str().expect("a pattern"))
                        .expect("the pattern compiles");
                    let max = schema["maxLength"].as_u64().expect("a maxLength") as usize;
                    assert!(fits(&pattern, max, text), "{path}: {text} does not fit");
                }
            }
            Some("integer") => {
                let n = value
                    .as_i64()
                    .map(i128::from)
                    .or(value.as_u64().map(i128::from))
                    .unwrap_or_else(|| panic!("{path}: not an integer"));
                if let Some(min) = schema["minimum"].as_i64() {
                    assert!(n >= i128::from(min), "{path}: below {min}");
                }
                if let Some(max) = schema["maximum"].as_u64() {
                    assert!(n <= i128::from(max), "{path}: above {max}");
                }
                if schema["format"] == "int32" {
                    assert!(i32::try_from(n).is_ok(), "{path}: not an int32");
                }
                if schema["format"] == "int64" {
                    assert!(i64::try_from(n).is_ok(), "{path}: not an int64");
                }
            }
            Some("boolean") => assert!(value.is_boolean(), "{path}: not a boolean"),
            other => panic!("{path}: unexpected schema type {other:?}"),
        }
    }

    #[test]
    fn what_kausa_sends_conforms_to_the_schema() {
        let s = schema();
        let mut no_controller = pod_spec();
        no_controller.sample.as_mut().expect("a sample").controller = None;
        let mut failed_read = pod_spec();
        failed_read.missing = Missing {
            owner_lookup_failed: true,
            revision_lookup_failed: true,
        };
        if let Some(c) = failed_read
            .sample
            .as_mut()
            .and_then(|s| s.controller.as_mut())
        {
            c.revision = None;
        }
        let name = Regex::new(INCIDENT_NAME_PATTERN).expect("the pattern compiles");
        for spec in [
            pod_spec(),
            event_spec(),
            rollout_spec(),
            longest_spec(),
            longest_pod_spec(),
            no_controller,
            failed_read,
        ] {
            let checked = spec.checked().expect("a spec that fits");
            conforms(
                &json!({"spec": checked.spec, "status": {}}),
                &s,
                &format!("{:?}", spec.detection),
            );
            assert!(name.is_match(&checked.name), "{}", checked.name);
            assert!(is_dns_subdomain(&checked.name), "{}", checked.name);
        }
    }

    #[test]
    fn the_spec_is_written_as_the_schema_names_it() {
        let checked = pod_spec().checked().expect("a spec that fits");
        assert_eq!(checked.name, "kausa.5d0c2b7e9a1f4c38.1795514400");
        assert_eq!(
            checked.spec,
            json!({
                "detection": "D001",
                "fingerprint": FP,
                "firstSeen": "2026-11-24T10:00:00Z",
                "settledAt": "2026-11-24T10:05:00Z",
                "object": {
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "name": "web",
                    "uid": UID_OWNER,
                },
                "facts": {
                    "reason": "CrashLoopBackOff",
                    "exitCode": 1,
                    "container": "app",
                    "podsSeen": 3,
                    "podsSeenIsMinimum": false,
                    "podsNow": 4,
                },
                "sample": {
                    "pod": {"name": "web-7d4b9c6f5d-abcde", "uid": UID_POD},
                    "controller": {"kind": "ReplicaSet", "uid": UID_RS, "revision": 7},
                },
                "missing": [],
            })
        );
        let mut spec = rollout_spec();
        spec.object.uid = None;
        let checked = spec.checked().expect("a spec that fits");
        assert_eq!(
            checked.spec["deployment"],
            json!({"generation": 12, "revision": 7})
        );
        assert_eq!(checked.spec["missing"], json!([]));
        assert!(checked.spec["object"].get("uid").is_none());
        let checked = unresolved_event_spec().checked().expect("a spec that fits");
        assert_eq!(checked.spec["missing"], json!(["owner-lookup-failed"]));
        assert!(checked.spec["facts"].get("exitCode").is_none());
        let checked = event_spec().checked().expect("a spec that fits");
        assert_eq!(
            checked.spec["regarding"],
            json!({"kind": "ReplicaSet", "name": "web-7d4b9c6f5d", "uid": UID_RS})
        );
        assert!(checked.spec.get("sample").is_none());
    }

    #[test]
    fn the_name_holds_the_first_seen_second_in_one_to_ten_digits() {
        let named = |first_seen: Timestamp| {
            let spec = Spec {
                first_seen,
                settled_at: first_seen,
                ..pod_spec()
            };
            spec.checked()
                .map(|c| (c.name, c.spec["firstSeen"].clone()))
        };
        assert_eq!(
            named(Timestamp::UNIX_EPOCH),
            Ok((format!("kausa.{FP}.0"), json!("1970-01-01T00:00:00Z")))
        );
        assert_eq!(
            named(seconds(NAME_SECONDS_END - 1)),
            Ok((
                format!("kausa.{FP}.9999999999"),
                json!("2286-11-20T17:46:39Z")
            ))
        );
        assert_eq!(seconds(NAME_SECONDS_END), at("2286-11-20T17:46:40Z"));
        assert_eq!(named(seconds(NAME_SECONDS_END)), Err(Field::Name));
        assert_eq!(named(seconds(-1)), Err(Field::Name));
        // Half a second before the epoch, its second is 0 but its time is 1969: refused.
        assert_eq!(named(at("1969-12-31T23:59:59.5Z")), Err(Field::Name));
        // The name's seconds are the firstSeen's, truncated.
        let (name, first_seen) = named(at("2026-11-24T10:00:00.999Z")).expect("a name");
        assert_eq!(name, format!("kausa.{FP}.1795514400"));
        assert_eq!(first_seen, json!("2026-11-24T10:00:00Z"));
    }

    #[test]
    fn times_are_written_in_utc_to_the_second() {
        assert_eq!(
            time(at("2026-11-24T10:00:00.999999999Z")),
            "2026-11-24T10:00:00Z"
        );
        assert_eq!(
            time(at("2026-11-24T12:00:00+02:00")),
            "2026-11-24T10:00:00Z"
        );
        assert_eq!(time(at("0001-01-01T00:00:00Z")), "0001-01-01T00:00:00Z");
        assert_eq!(time(Timestamp::MAX), "9999-12-30T22:00:00Z");
        assert!(is_time(&time(Timestamp::MAX)));
        assert!(!is_time(&time(Timestamp::MIN)), "{}", time(Timestamp::MIN));
        assert!(!is_time(&time(seconds(-62_167_219_201))), "a negative year");
    }

    #[test]
    fn facts_of_a_statement_cap_the_pods_seen() {
        let statement = |pods_seen: usize| fact::Facts {
            detection: Detection::CrashLoop,
            container: Some("app".into()),
            reason: Some("Error"),
            exit_code: Some(137),
            pods_seen,
            pods_seen_is_minimum: pods_seen >= MAX_PODS,
            pods_now: 200,
            revision: None,
        };
        for (seen, written, minimum) in [
            (1, 1, false),
            (99, 99, false),
            (100, 100, true),
            (101, 100, true),
        ] {
            let facts = Facts::of(&statement(seen));
            assert_eq!(
                (facts.pods_seen, facts.pods_seen_is_minimum),
                (written, minimum),
                "{seen}"
            );
            assert_eq!(facts.reason, Some("Error"));
            assert_eq!(facts.exit_code, Some(137));
            assert_eq!(facts.container.as_deref(), Some("app"));
            assert_eq!(facts.pods_now, 200);
            let spec = Spec {
                facts,
                ..pod_spec()
            };
            assert_eq!(spec.check(), Ok(()), "{seen}");
        }
    }

    #[test]
    fn a_uid_is_kept_as_its_sixteen_bytes() {
        assert_eq!(
            Uid::parse("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"),
            Some(Uid([
                0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3, 0xd2,
                0xe1, 0xf0
            ]))
        );
        for text in [
            "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f",
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0a",
            "0f1e2d3c-4b5a-6978-8796_a5b4c3d2e1f0",
            "",
        ] {
            assert_eq!(Uid::parse(text), None, "{text}");
        }
    }

    #[test]
    fn a_controller_kind_not_listed_is_other() {
        for kind in ControllerKind::ALL {
            assert_eq!(
                ControllerKind::of(kind.as_str()),
                if kind == ControllerKind::Other {
                    ControllerKind::Other
                } else {
                    kind
                }
            );
        }
        for kind in ["ReplicationController", "replicaset", "", "Other"] {
            assert_eq!(ControllerKind::of(kind), ControllerKind::Other, "{kind}");
        }
    }

    #[test]
    fn the_longest_specs_stay_near_the_adr_s_estimate() {
        // 0028 estimates about 1.5 KB for the spec at the longest names; its bar 6 measures the
        // stored object. Pinned here as the JSON kausa sends: 1,364 bytes for an incident about
        // another object and 1,460 for one about Pods. The figures come first, so that a failure
        // of the bound prints them.
        let bytes = |spec: Spec| {
            serde_json::to_vec(&spec.checked().expect("fits").spec)
                .expect("JSON")
                .len()
        };
        let longest = (bytes(longest_spec()), bytes(longest_pod_spec()));
        assert_eq!(longest, (1364, 1460));
        assert!(longest.0.max(longest.1) <= 1536);
    }
}
