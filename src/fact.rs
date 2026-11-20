//! The fact statement of an incident: one line of text that says what kausa read, and nothing a
//! tenant or a model wrote (docs/adr/0027-fact-statement-for-the-detections.md, point 1).
//!
//! Prototype of a proposed ADR, on a branch that is not merged: it exists to measure the bounds of
//! the ADR's point 10.
//!
//! Pure: no I/O, no clock, no state. The same incident and the same `pods_total` give the same
//! bytes. Every value is either fixed in the binary (the detection's name and id, a reason of the
//! list below), an integer, or a container name that is a DNS label, checked here. The sample's
//! message, an event's note, the image, a label, a node, the namespace, the owner and the Pod are
//! never in it.

use crate::detect::Detection;
use crate::incident::{Incident, MAX_PODS};

/// The bound of a statement, in bytes. `LONGEST_STATEMENT_BYTES` is the longest one possible.
pub const MAX_STATEMENT_BYTES: usize = 512;

/// The longest statement that can be built, in bytes: every part at its longest, pinned by a test.
pub const LONGEST_STATEMENT_BYTES: usize = 286;

// Under the bound, which is itself under the limit of an Event's note.
const _: () = assert!(LONGEST_STATEMENT_BYTES < MAX_STATEMENT_BYTES);
const _: () = assert!(MAX_STATEMENT_BYTES <= crate::events::MAX_NOTE_BYTES);

/// The longest container name a statement prints: a DNS label.
const MAX_LABEL_BYTES: usize = 63;

/// The reasons a statement of `detection` may print: those of docs/reference/rules.md, and `Error`
/// for a crash loop found by its restart count. Any other value is left out and counted.
fn reasons(detection: Detection) -> &'static [&'static str] {
    match detection {
        Detection::CrashLoop => &["CrashLoopBackOff", "Error"],
        Detection::ImagePull => &[
            "ErrImagePull",
            "ImagePullBackOff",
            "InvalidImageName",
            "ErrImageNeverPull",
        ],
        Detection::OutOfMemory => &["OOMKilled"],
        Detection::ContainerCreate => &[
            "CreateContainerConfigError",
            "CreateContainerError",
            "RunContainerError",
        ],
        Detection::Unschedulable => &["Unschedulable", "SchedulerError"],
        Detection::ProbeFailure => &["Unhealthy"],
        Detection::VolumeFailure => &["FailedMount", "FailedAttachVolume"],
        Detection::JobFailed => &[
            "BackoffLimitExceeded",
            "DeadlineExceeded",
            "PodFailurePolicy",
            "MaxFailedIndexesExceeded",
            "FailedIndexes",
        ],
        Detection::PodEvicted => &["Evicted"],
        Detection::SandboxCreate => &["FailedCreatePodSandBox"],
        Detection::PodCreate => &["FailedCreate"],
        Detection::ClaimProvisioning => &["ProvisioningFailed"],
        Detection::AutoscalerMetrics => &[
            "FailedGetResourceMetric",
            "FailedGetContainerResourceMetric",
            "FailedGetPodsMetric",
            "FailedGetObjectMetric",
            "FailedGetExternalMetric",
            "FailedComputeMetricsReplicas",
            "FailedGetScale",
        ],
        Detection::RolloutStalled => &["ProgressDeadlineExceeded"],
    }
}

/// Whether `name` is a DNS label as Kubernetes defines one: 1 to 63 bytes of lowercase letters,
/// digits and `-`, starting and ending with a letter or a digit.
fn is_dns_label(name: &str) -> bool {
    let bytes = name.as_bytes();
    let alphanumeric = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    !bytes.is_empty()
        && bytes.len() <= MAX_LABEL_BYTES
        && bytes.iter().all(|b| alphanumeric(b) || *b == b'-')
        && bytes.first().is_some_and(alphanumeric)
        && bytes.last().is_some_and(alphanumeric)
}

/// The values a statement is rendered from: only what its allowlist let through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub detection: Detection,
    /// The sample's container, when its name is a DNS label.
    pub container: Option<String>,
    /// The sample's reason, as the entry of the list it equals.
    pub reason: Option<&'static str>,
    pub exit_code: Option<i32>,
    /// How many names the incident's Pod set holds.
    pub pods_seen: usize,
    /// Whether `pods_seen` is a lower bound: the set holds `MAX_PODS` names or more, and the first
    /// `MAX_PODS` seen while the incident was pending are a cap.
    pub pods_seen_is_minimum: bool,
    /// The Pods kausa holds with the incident's owner (`pods_total` of the `incident` line).
    pub pods_now: usize,
    /// For `D014`, the revision of the Deployment's `revision <n>`.
    pub revision: Option<u64>,
}

/// A statement and what was left out of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    pub text: String,
    pub facts: Facts,
    /// Whether the sample carried a reason outside the list, which the statement leaves out
    /// (`kausa_fact_reasons_omitted_total`).
    pub reason_omitted: bool,
}

/// The fact statement of `incident`, with `pods_total` Pods held for its owner.
pub fn statement(incident: &Incident, pods_total: usize) -> Statement {
    let detection = incident.key.detection;
    let sample = &incident.sample;
    let reason = reasons(detection)
        .iter()
        .copied()
        .find(|allowed| *allowed == sample.reason);
    let pods_seen = incident.pods.len();
    let facts = Facts {
        detection,
        container: sample
            .container
            .as_deref()
            .filter(|name| is_dns_label(name))
            .map(str::to_owned),
        reason,
        exit_code: sample.exit_code,
        pods_seen,
        pods_seen_is_minimum: pods_seen >= MAX_PODS,
        pods_now: pods_total,
        revision: (detection == Detection::RolloutStalled)
            .then(|| sample.message.strip_prefix("revision "))
            .flatten()
            .and_then(|n| n.parse().ok()),
    };
    let text = render(&facts);
    debug_assert!(text.len() <= LONGEST_STATEMENT_BYTES, "{text}");
    Statement {
        text,
        reason_omitted: reason.is_none() && !sample.reason.is_empty(),
        facts,
    }
}

/// The text of `facts`: its parts in a fixed order, each left out when its value is absent.
pub fn render(facts: &Facts) -> String {
    let detection = facts.detection;
    let mut text = format!("{} ({}).", detection.name(), detection.id());
    let mut sample = Vec::new();
    if let Some(container) = &facts.container {
        sample.push(format!("container {container}"));
    }
    if let Some(reason) = facts.reason {
        sample.push(format!("reason {reason}"));
    }
    if let Some(code) = facts.exit_code {
        sample.push(format!("exit code {code}"));
    }
    if !sample.is_empty() {
        text.push_str(&format!(" Sample: {}.", sample.join(", ")));
    }
    if facts.pods_seen > 0 {
        let at_least = if facts.pods_seen_is_minimum {
            "at least "
        } else {
            ""
        };
        text.push_str(&format!(
            " Pods seen failing: {at_least}{}. Pods now: {}.",
            facts.pods_seen, facts.pods_now
        ));
    }
    if let Some(revision) = facts.revision {
        text.push_str(&format!(" Revision: {revision}."));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::{IncidentKey, Signal};
    use crate::record::OwnerRef;
    use k8s_openapi::jiff::Timestamp;

    /// Every detection, in the order of the enum.
    const ALL: [Detection; 14] = [
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

    /// An incident of `detection` with `pods` Pods and a sample of the given reason.
    fn incident(detection: Detection, reason: &str, pods: usize) -> Incident {
        let owner = OwnerRef {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "web".into(),
            uid: "uid-web".into(),
        };
        let key = IncidentKey {
            namespace: "shop".into(),
            owner_kind: owner.kind.clone(),
            owner_name: owner.name.clone(),
            detection,
        };
        let at = Timestamp::from_second(1_800_000_000).expect("a valid time");
        Incident {
            sample: Signal {
                key: key.clone(),
                owner: owner.clone(),
                observed_at: at,
                event: None,
                regarding_kind: None,
                pod: (pods > 0).then(|| "web-0".to_owned()),
                container: None,
                image: None,
                reason: reason.into(),
                message: String::new(),
                exit_code: None,
            },
            key,
            owner,
            fingerprint: "5d0c2b7e9a1f4c38".into(),
            first_seen: at,
            last_seen: at,
            pods: (0..pods).map(|i| format!("web-{i}")).collect(),
        }
    }

    fn allowed_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b" .,:()-".contains(&b)
    }

    #[test]
    fn the_examples_of_the_adr() {
        let mut crash = incident(Detection::CrashLoop, "CrashLoopBackOff", 2);
        crash.sample.container = Some("web".into());
        crash.sample.exit_code = Some(1);
        assert_eq!(
            statement(&crash, 3).text,
            "crash loop (D001). Sample: container web, reason CrashLoopBackOff, exit code 1. Pods \
             seen failing: 2. Pods now: 3."
        );

        let mut pull = incident(Detection::ImagePull, "ImagePullBackOff", 1);
        pull.sample.container = Some("web".into());
        assert_eq!(
            statement(&pull, 1).text,
            "image cannot be pulled (D002). Sample: container web, reason ImagePullBackOff. Pods \
             seen failing: 1. Pods now: 1."
        );

        let job = incident(Detection::JobFailed, "BackoffLimitExceeded", 0);
        assert_eq!(
            statement(&job, 0).text,
            "job failed (D008). Sample: reason BackoffLimitExceeded."
        );

        let mut rollout = incident(Detection::RolloutStalled, "ProgressDeadlineExceeded", 0);
        rollout.sample.message = "revision 7".into();
        assert_eq!(
            statement(&rollout, 4).text,
            "rollout does not progress (D014). Sample: reason ProgressDeadlineExceeded. Revision: \
             7."
        );
    }

    #[test]
    fn the_same_incident_gives_the_same_bytes() {
        let mut crash = incident(Detection::CrashLoop, "CrashLoopBackOff", 2);
        crash.sample.container = Some("web".into());
        assert_eq!(statement(&crash, 3), statement(&crash, 3));
    }

    #[test]
    fn free_text_never_reaches_a_statement() {
        let mut crash = incident(Detection::CrashLoop, "CrashLoopBackOff", 1);
        crash.sample.message = "back-off 5m0s <@everyone> http://example.com `rm -rf /`".into();
        crash.sample.image = Some("registry.example.com/web:1 <b>".into());
        crash.sample.pod = Some("web-0".into());
        let text = statement(&crash, 1).text;
        assert_eq!(
            text,
            "crash loop (D001). Sample: reason CrashLoopBackOff. Pods seen failing: 1. Pods now: 1."
        );
        // Neither the namespace, the owner nor the Pod.
        for name in ["shop", "web", "Deployment"] {
            assert!(!text.contains(name), "{name} is in {text}");
        }
    }

    #[test]
    fn a_container_name_that_is_not_a_dns_label_is_left_out() {
        for name in [
            "",
            "Web",
            "web_1",
            "-web",
            "web-",
            "web.1",
            "web 1",
            "wéb",
            "<@everyone>",
            &"a".repeat(64),
        ] {
            let mut crash = incident(Detection::CrashLoop, "CrashLoopBackOff", 1);
            crash.sample.container = Some(name.into());
            let built = statement(&crash, 1);
            assert_eq!(built.facts.container, None, "{name:?}");
            assert!(!built.text.contains("container"), "{name:?}");
        }
        for name in ["a", "web", "web-1", "0", &"a".repeat(63)] {
            let mut crash = incident(Detection::CrashLoop, "CrashLoopBackOff", 1);
            crash.sample.container = Some(name.into());
            assert_eq!(
                statement(&crash, 1).facts.container.as_deref(),
                Some(name),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_reason_outside_the_list_is_left_out_and_said_so() {
        // The restart-count path of D001 carries whatever the runtime wrote.
        let mut crash = incident(Detection::CrashLoop, "ContainerCannotRun <@here>", 1);
        crash.sample.exit_code = Some(127);
        let built = statement(&crash, 1);
        assert!(built.reason_omitted);
        assert_eq!(
            built.text,
            "crash loop (D001). Sample: exit code 127. Pods seen failing: 1. Pods now: 1."
        );
        // `Error` is the one terminated reason of the list, and only for D001.
        let error = statement(&incident(Detection::CrashLoop, "Error", 1), 1);
        assert_eq!(
            (error.facts.reason, error.reason_omitted),
            (Some("Error"), false)
        );
        let elsewhere = statement(&incident(Detection::ImagePull, "Error", 1), 1);
        assert_eq!(
            (elsewhere.facts.reason, elsewhere.reason_omitted),
            (None, true)
        );
        // No reason at all is absent, not omitted.
        let none = statement(&incident(Detection::CrashLoop, "", 1), 1);
        assert_eq!((none.facts.reason, none.reason_omitted), (None, false));
        assert_eq!(
            none.text,
            "crash loop (D001). Pods seen failing: 1. Pods now: 1."
        );
    }

    #[test]
    fn every_listed_reason_is_printed_for_its_detection() {
        for detection in ALL {
            assert!(!reasons(detection).is_empty(), "{detection:?}");
            for reason in reasons(detection) {
                let built = statement(&incident(detection, reason, 0), 0);
                assert!(!built.reason_omitted, "{detection:?} {reason}");
                assert_eq!(
                    built.text,
                    format!(
                        "{} ({}). Sample: reason {reason}.",
                        detection.name(),
                        detection.id()
                    )
                );
            }
        }
    }

    #[test]
    fn the_pod_set_at_its_cap_is_a_lower_bound() {
        let under = statement(&incident(Detection::CrashLoop, "Error", 99), 120);
        assert!(
            under
                .text
                .ends_with(" Pods seen failing: 99. Pods now: 120.")
        );
        assert!(!under.facts.pods_seen_is_minimum);
        let at = statement(&incident(Detection::CrashLoop, "Error", 100), 120);
        assert!(
            at.text
                .ends_with(" Pods seen failing: at least 100. Pods now: 120.")
        );
        assert!(at.facts.pods_seen_is_minimum);
        let over = statement(&incident(Detection::CrashLoop, "Error", 101), 120);
        assert!(
            over.text
                .ends_with(" Pods seen failing: at least 101. Pods now: 120.")
        );
    }

    #[test]
    fn the_revision_is_an_unsigned_integer_of_d014_only() {
        let mut rollout = incident(Detection::RolloutStalled, "ProgressDeadlineExceeded", 0);
        for message in [
            "",
            "revision",
            "revision -1",
            "revision 7 <b>",
            "revision x",
            "7",
        ] {
            rollout.sample.message = message.into();
            assert_eq!(statement(&rollout, 0).facts.revision, None, "{message:?}");
        }
        rollout.sample.message = format!("revision {}", u64::MAX);
        assert_eq!(statement(&rollout, 0).facts.revision, Some(u64::MAX));
        let mut crash = incident(Detection::CrashLoop, "Error", 1);
        crash.sample.message = "revision 7".into();
        assert_eq!(statement(&crash, 1).facts.revision, None);
    }

    /// Every part at its longest, for each detection and each of its reasons.
    fn longest(detection: Detection, reason: &'static str) -> Facts {
        Facts {
            detection,
            container: Some("a".repeat(MAX_LABEL_BYTES)),
            reason: Some(reason),
            exit_code: Some(i32::MIN),
            pods_seen: usize::MAX,
            pods_seen_is_minimum: true,
            pods_now: usize::MAX,
            revision: (detection == Detection::RolloutStalled).then_some(u64::MAX),
        }
    }

    #[test]
    fn the_longest_statement_is_pinned_and_under_the_bound() {
        let mut most = 0;
        for detection in ALL {
            for reason in reasons(detection) {
                let text = render(&longest(detection, reason));
                assert!(text.bytes().all(allowed_byte), "{text}");
                most = most.max(text.len());
            }
        }
        // Wider than any incident gives: a rollout has no container, exit code or Pod set, and
        // only a rollout has a revision.
        assert_eq!(most, LONGEST_STATEMENT_BYTES);
    }

    #[test]
    fn only_the_allowed_bytes_whatever_the_sample_holds() {
        let hostile = [
            "<@everyone>",
            "http://example.com/x",
            "`rm -rf /`",
            "a\nb",
            "a\u{202e}b",
            "*bold* _x_ [l](u)",
            "",
        ];
        for detection in ALL {
            for text in hostile {
                let mut built = incident(detection, text, 3);
                built.sample.container = Some(text.into());
                built.sample.message = text.into();
                built.sample.image = Some(text.into());
                built.sample.exit_code = Some(-1);
                let statement = statement(&built, 5).text;
                assert!(statement.bytes().all(allowed_byte), "{statement:?}");
                assert!(statement.len() < MAX_STATEMENT_BYTES);
            }
        }
    }
}
