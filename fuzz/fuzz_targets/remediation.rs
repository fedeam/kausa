//! The parse and the checks of a kurita Remediation (src/remediation.rs), of any JSON: they must
//! never panic, a reported record's message holds only the values of kausa's own Incident and a
//! fixed action word, and the `remediation ignored` line gives a namespace and a name only when
//! each fits its pattern (docs/adr/0028-incident-resource-and-remediation-report.md, point 6).
//! The seeds in fuzz/seeds/remediation/ hold the record that is reported and one per reason a
//! record is ignored.
#![no_main]

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/record.rs"]
mod record;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/detect.rs"]
mod detect;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/fingerprint.rs"]
mod fingerprint;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/incident.rs"]
mod incident;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/owner.rs"]
mod owner;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/fact.rs"]
mod fact;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/incident_resource.rs"]
mod incident_resource;

#[allow(dead_code)] // floor-ok: the mod holds more than the fuzz target uses, like its neighbours
#[path = "../../src/remediation.rs"]
mod remediation;

use std::sync::LazyLock;
use std::time::Instant;

use k8s_openapi::jiff::Timestamp;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

use detect::Detection;
use incident_resource::table::{Entry, Table};
use incident_resource::{
    Controller, ControllerKind, Facts, Missing, Object, Sample, Spec, Uid, time,
};
use remediation::{Action, Limiter, Outcome, check, parse};

const NAMESPACE: &str = "shop";
const FINGERPRINT: &str = "5d0c2b7e9a1f4c38";

fn at(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

/// One Incident, a crash loop of `shop/web`, that the fuzzer may name.
static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let spec = Spec {
        detection: Detection::CrashLoop,
        fingerprint: FINGERPRINT.into(),
        first_seen: at("2026-11-24T10:00:00Z"),
        settled_at: at("2026-11-24T10:01:00Z"),
        object: Object {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "web".into(),
            uid: Some("00000000-0000-4000-8000-000000000003".into()),
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
            pod_uid: "00000000-0000-4000-8000-000000000001".into(),
            controller: Some(Controller {
                kind: ControllerKind::ReplicaSet,
                uid: "00000000-0000-4000-8000-000000000002".into(),
                revision: Some(7),
            }),
        }),
        deployment: None,
        regarding: None,
        missing: Missing::default(),
    };
    let uid = Uid::parse("00000000-0000-4000-8000-0000000000aa").expect("a UID");
    let entry = Entry::new(NAMESPACE, &spec, uid, Instant::now()).expect("an entry");
    let mut table = Table::default();
    table.insert(entry, Instant::now()).expect("room");
    table
});

fuzz_target!(|data: &[u8]| {
    let Ok(object) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    let now = at("2026-11-24T10:05:00Z");
    let parsed = parse(&object);
    match check(&object, &TABLE, now) {
        Outcome::Report(report) => {
            assert!(parsed.is_some());
            assert_ne!(report.action(), Action::None);
            let proposal = report.proposal();
            assert_eq!(proposal.detection_id(), "D001");
            assert_eq!(proposal.namespace(), NAMESPACE);
            assert_eq!(proposal.fingerprint(), FINGERPRINT);
            assert_eq!(proposal.api_version(), "apps/v1");
            assert_eq!(proposal.kind(), "Deployment");
            assert_eq!(proposal.name(), "web");
            assert_eq!(proposal.action(), report.action().as_str());
            assert_eq!(proposal.time(), time(report.proposed_at()));
            assert!(report.proposed_at() <= now);
        }
        Outcome::Nothing => assert!(parsed.is_some()),
        Outcome::Ignored(reason) => {
            let mut limiter = Limiter::default();
            let line = limiter
                .line(reason, &object, Instant::now())
                .expect("the first line of a reason");
            if let Some(namespace) = line.namespace {
                assert!(owner::is_dns_label(namespace));
            }
            if let Some(name) = line.name {
                assert!(name.starts_with("kurita.") && name.is_ascii() && name.len() <= 34);
            }
        }
    }
});
