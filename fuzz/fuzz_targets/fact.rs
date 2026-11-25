//! The fact statement (src/fact.rs) of any incident: building it must never panic, and it is
//! ASCII made only of the bytes its fixed parts, integers and DNS labels use, within
//! `LONGEST_STATEMENT_BYTES`. A container name is kept only when it is a DNS label and a reason
//! only when it is on the detection's list; the sample's message, the namespace, the owner and the
//! Pods never reach it (docs/adr/0027-fact-statement-for-the-detections.md).
#![no_main]

#[allow(dead_code)]
#[path = "../../src/record.rs"]
mod record;

#[allow(dead_code)]
#[path = "../../src/detect.rs"]
mod detect;

#[allow(dead_code)]
#[path = "../../src/fingerprint.rs"]
mod fingerprint;

#[allow(dead_code)]
#[path = "../../src/incident.rs"]
mod incident;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/owner.rs"]
mod owner;

#[allow(dead_code)]
#[path = "../../src/fact.rs"]
mod fact;

use std::collections::BTreeSet;

use k8s_openapi::jiff::Timestamp;
use libfuzzer_sys::fuzz_target;

use detect::Detection;
use fact::{LONGEST_STATEMENT_BYTES, MAX_STATEMENT_BYTES};
use incident::{Incident, IncidentKey, Signal};
use record::OwnerRef;

const DETECTIONS: [Detection; 14] = [
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

/// The fuzzer's input: a detection pick, the sample's container, reason and message, its exit
/// code, the names of the object and the Pods, how many times the Pod names are numbered (so that
/// the set can pass `MAX_PODS` within libFuzzer's input size), and the Pods held.
type Input<'a> = (
    u8,
    (Option<&'a str>, &'a str, &'a str, Option<i32>),
    (&'a str, &'a str, &'a str),
    Vec<&'a str>,
    u8,
    usize,
);

fn allowed(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b" .,:()-".contains(&b)
}

fuzz_target!(|input: Input| {
    let (pick, (container, reason, message, exit_code), (namespace, kind, name), pods, repeat, pods_total) =
        input;
    let detection = DETECTIONS[usize::from(pick) % DETECTIONS.len()];
    let mut set: BTreeSet<String> = pods.iter().map(|p| (*p).to_owned()).collect();
    for i in 0..usize::from(repeat) {
        set.insert(format!("{}-{i}", pods.first().copied().unwrap_or("pod")));
    }
    let owner = OwnerRef {
        api_version: "apps/v1".to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
        uid: "00000000-0000-0000-0000-000000000000".to_owned(),
    };
    let key = IncidentKey {
        namespace: namespace.to_owned(),
        owner_kind: kind.to_owned(),
        owner_name: name.to_owned(),
        detection,
    };
    let incident = Incident {
        key: key.clone(),
        owner: owner.clone(),
        fingerprint: "0123456789abcdef".to_owned(),
        first_seen: Timestamp::UNIX_EPOCH,
        last_seen: Timestamp::UNIX_EPOCH,
        pods: set.clone(),
        sample: Signal {
            key,
            owner,
            observed_at: Timestamp::UNIX_EPOCH,
            event: None,
            regarding_kind: None,
            pod: set.iter().next().cloned(),
            pod_uid: None,
            container: container.map(str::to_owned),
            image: None,
            reason: reason.to_owned(),
            message: message.to_owned(),
            exit_code,
        },
    };

    let statement = fact::statement(&incident, pods_total);
    let text = &statement.text;
    assert!(text.len() <= LONGEST_STATEMENT_BYTES, "{} bytes", text.len());
    assert!(text.len() < MAX_STATEMENT_BYTES);
    assert!(text.bytes().all(allowed), "a byte outside the statement's: {text:?}");
    assert_eq!(*text, fact::render(&statement.facts), "the text is not its facts'");

    let facts = &statement.facts;
    assert_eq!(facts.detection, detection);
    assert_eq!(facts.pods_seen, set.len());
    assert_eq!(facts.pods_now, pods_total);
    assert_eq!(facts.exit_code, exit_code);
    if let Some(kept) = &facts.container {
        assert_eq!(Some(kept.as_str()), container, "a container that is not the sample's");
        assert!(kept.len() <= 63 && kept.bytes().all(allowed), "not a DNS label: {kept:?}");
    }
    match facts.reason {
        Some(kept) => assert_eq!(kept, reason, "a reason that is not the sample's"),
        None => assert_eq!(statement.reason_omitted, !reason.is_empty()),
    }
    if facts.reason.is_some() {
        assert!(!statement.reason_omitted);
    }
    if let Some(revision) = facts.revision {
        assert_eq!(detection, Detection::RolloutStalled);
        assert_eq!(message.strip_prefix("revision ").map(str::parse), Some(Ok(revision)));
    }
});
