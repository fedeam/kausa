//! The detection rules: which container states and Warning events are failures kausa explains.
//!
//! The catalogue, with ids and triggers, is docs/reference/rules.md. Rules are pure functions of a
//! record: no I/O, no clock, no state.

use crate::record::{ContainerRecord, EventRecord, PodRecord};

/// A detection rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Detection {
    /// `D001`: the container is waiting in `CrashLoopBackOff`.
    CrashLoop,
    /// `D002`: the image cannot be pulled.
    ImagePull,
    /// `D003`: the container was killed for exceeding its memory limit.
    OutOfMemory,
    /// `D004`: the container cannot be created (a missing Secret or ConfigMap key, for example).
    ContainerCreate,
    /// `D005`: the Pod cannot be scheduled.
    Unschedulable,
    /// `D006`: a probe keeps failing.
    ProbeFailure,
    /// `D007`: a volume cannot be attached or mounted.
    VolumeFailure,
    /// `D008`: a Job failed.
    JobFailed,
}

impl Detection {
    pub fn id(self) -> &'static str {
        match self {
            Detection::CrashLoop => "D001",
            Detection::ImagePull => "D002",
            Detection::OutOfMemory => "D003",
            Detection::ContainerCreate => "D004",
            Detection::Unschedulable => "D005",
            Detection::ProbeFailure => "D006",
            Detection::VolumeFailure => "D007",
            Detection::JobFailed => "D008",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Detection::CrashLoop => "crash loop",
            Detection::ImagePull => "image cannot be pulled",
            Detection::OutOfMemory => "out of memory",
            Detection::ContainerCreate => "container cannot be created",
            Detection::Unschedulable => "pod cannot be scheduled",
            Detection::ProbeFailure => "probe failures",
            Detection::VolumeFailure => "volume cannot be attached or mounted",
            Detection::JobFailed => "job failed",
        }
    }

    /// Whether the failure has to persist through the settle window before it is an incident. A
    /// failed Job is final, so it is an incident at once.
    pub fn settles(self) -> bool {
        self != Detection::JobFailed
    }

    /// Whether the signal is a state that kausa reads from the Pod at every check (container
    /// states, the Pod's scheduling condition), rather than an event.
    pub fn is_state(self) -> bool {
        matches!(
            self,
            Detection::CrashLoop
                | Detection::ImagePull
                | Detection::OutOfMemory
                | Detection::ContainerCreate
                | Detection::Unschedulable
        )
    }
}

/// Whether the Pod cannot be scheduled: still `Pending`, with its `PodScheduled` condition `False`
/// for reason `Unschedulable`. The scheduler's `FailedScheduling` events say the same, but they
/// may stop being updated for a Pod that keeps failing, while the condition holds as long as the
/// failure: the condition is the signal.
pub fn detect_unschedulable(pod: &PodRecord) -> bool {
    pod.phase.as_deref() == Some("Pending") && pod.unschedulable.is_some()
}

/// The detection that a container's state matches, if any. A container matches at most one: an
/// out-of-memory kill takes precedence over the crash loop it causes.
pub fn detect_container(container: &ContainerRecord) -> Option<Detection> {
    let oom = |reason: &Option<String>| reason.as_deref() == Some("OOMKilled");
    match container.waiting_reason.as_deref() {
        Some("CrashLoopBackOff") if oom(&container.last_termination_reason) => {
            Some(Detection::OutOfMemory)
        }
        Some("CrashLoopBackOff") => Some(Detection::CrashLoop),
        Some("ErrImagePull" | "ImagePullBackOff" | "InvalidImageName") => {
            Some(Detection::ImagePull)
        }
        Some("CreateContainerConfigError" | "CreateContainerError") => {
            Some(Detection::ContainerCreate)
        }
        // A container that does not restart (`restartPolicy: Never` or `OnFailure` in a Job's Pod
        // that has given up) stays terminated with the reason of its kill.
        _ if oom(&container.terminated_reason) => Some(Detection::OutOfMemory),
        // Between restarts the kubelet reports a crash-looping container as terminated (reason
        // `Error`) for much of each back-off, and as waiting in `CrashLoopBackOff` only part of
        // the time (seen on Kubernetes 1.36 on 2026-10-26). A failed container that has already
        // restarted is the same crash loop.
        _ if container.restart_count > 0
            && container
                .terminated_reason
                .as_deref()
                .is_some_and(|reason| reason != "Completed") =>
        {
            Some(Detection::CrashLoop)
        }
        _ => None,
    }
}

/// The detection that a Warning event matches, if any. Events about Pods and Jobs only; scheduling
/// failures come from the Pod's condition instead (`detect_unschedulable`).
pub fn detect_event(event: &EventRecord) -> Option<Detection> {
    match (event.regarding.kind.as_str(), event.reason.as_str()) {
        ("Pod", "Unhealthy") => Some(Detection::ProbeFailure),
        ("Pod", "FailedMount" | "FailedAttachVolume") => Some(Detection::VolumeFailure),
        // The Job controller emits these as Warning events on the Job when it marks it failed
        // (pkg/controller/job/job_controller.go, checked on 2026-10-26).
        ("Job", "BackoffLimitExceeded" | "DeadlineExceeded") => Some(Detection::JobFailed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explain::reads_logs;
    use crate::record::Regarding;

    fn waiting(reason: &str) -> ContainerRecord {
        ContainerRecord {
            name: "app".into(),
            waiting_reason: Some(reason.into()),
            ..Default::default()
        }
    }

    fn event(kind: &str, reason: &str) -> EventRecord {
        EventRecord {
            uid: "e1".into(),
            reason: reason.into(),
            note: None,
            regarding: Regarding {
                api_version: "v1".into(),
                kind: kind.into(),
                namespace: "shop".into(),
                name: "x".into(),
                uid: None,
            },
            reporting_controller: None,
            first_seen: None,
            last_seen: None,
            count: 1,
        }
    }

    #[test]
    fn ids_are_unique_and_in_order() {
        let all = [
            Detection::CrashLoop,
            Detection::ImagePull,
            Detection::OutOfMemory,
            Detection::ContainerCreate,
            Detection::Unschedulable,
            Detection::ProbeFailure,
            Detection::VolumeFailure,
            Detection::JobFailed,
        ];
        let ids: Vec<_> = all.iter().map(|d| d.id()).collect();
        assert_eq!(
            ids,
            [
                "D001", "D002", "D003", "D004", "D005", "D006", "D007", "D008"
            ]
        );
    }

    #[test]
    fn container_states() {
        assert_eq!(
            detect_container(&waiting("CrashLoopBackOff")),
            Some(Detection::CrashLoop)
        );
        for reason in ["ErrImagePull", "ImagePullBackOff", "InvalidImageName"] {
            assert_eq!(
                detect_container(&waiting(reason)),
                Some(Detection::ImagePull),
                "{reason}"
            );
        }
        for reason in ["CreateContainerConfigError", "CreateContainerError"] {
            assert_eq!(
                detect_container(&waiting(reason)),
                Some(Detection::ContainerCreate),
                "{reason}"
            );
        }
    }

    #[test]
    fn out_of_memory_takes_precedence_over_the_crash_loop() {
        let mut container = waiting("CrashLoopBackOff");
        container.last_termination_reason = Some("OOMKilled".into());
        assert_eq!(detect_container(&container), Some(Detection::OutOfMemory));
    }

    #[test]
    fn a_failed_container_that_restarted_is_a_crash_loop() {
        let between_back_offs = ContainerRecord {
            terminated_reason: Some("Error".into()),
            terminated_exit_code: Some(1),
            restart_count: 3,
            ..Default::default()
        };
        assert_eq!(
            detect_container(&between_back_offs),
            Some(Detection::CrashLoop)
        );
        // Not a crash loop: the first failure of a Pod that never restarts (a Job's, handled by
        // D008), and a container that completed.
        let first_failure = ContainerRecord {
            restart_count: 0,
            ..between_back_offs.clone()
        };
        assert_eq!(detect_container(&first_failure), None);
        let completed = ContainerRecord {
            terminated_reason: Some("Completed".into()),
            ..between_back_offs
        };
        assert_eq!(detect_container(&completed), None);
    }

    #[test]
    fn a_terminated_container_killed_for_memory_is_out_of_memory() {
        let container = ContainerRecord {
            terminated_reason: Some("OOMKilled".into()),
            ..Default::default()
        };
        assert_eq!(detect_container(&container), Some(Detection::OutOfMemory));
    }

    #[test]
    fn healthy_and_ordinary_states_match_nothing() {
        // Starting containers, a container that recovered after one OOM kill, a completed one.
        assert_eq!(detect_container(&waiting("ContainerCreating")), None);
        assert_eq!(detect_container(&waiting("PodInitializing")), None);
        let recovered = ContainerRecord {
            last_termination_reason: Some("OOMKilled".into()),
            ..Default::default()
        };
        assert_eq!(detect_container(&recovered), None);
        let completed = ContainerRecord {
            terminated_reason: Some("Completed".into()),
            ..Default::default()
        };
        assert_eq!(detect_container(&completed), None);
    }

    #[test]
    fn events() {
        assert_eq!(
            detect_event(&event("Pod", "Unhealthy")),
            Some(Detection::ProbeFailure)
        );
        assert_eq!(
            detect_event(&event("Pod", "FailedMount")),
            Some(Detection::VolumeFailure)
        );
        assert_eq!(
            detect_event(&event("Job", "BackoffLimitExceeded")),
            Some(Detection::JobFailed)
        );
        assert_eq!(
            detect_event(&event("Job", "DeadlineExceeded")),
            Some(Detection::JobFailed)
        );
    }

    #[test]
    fn events_about_other_objects_or_reasons_match_nothing() {
        // The kubelet's own back-off event duplicates the container state D001 already reads.
        assert_eq!(detect_event(&event("Pod", "BackOff")), None);
        // Scheduling failures are read from the Pod's condition, not from these events.
        assert_eq!(detect_event(&event("Pod", "FailedScheduling")), None);
        assert_eq!(detect_event(&event("Node", "FailedMount")), None);
        assert_eq!(detect_event(&event("Deployment", "FailedCreate")), None);
    }

    #[test]
    fn a_sandbox_that_cannot_be_created_is_d010() {
        // The kubelet's Warning event when the runtime cannot create the Pod's sandbox (a missing
        // RuntimeClass handler on kind; CNI failures give the same reason).
        let detection = detect_event(&event("Pod", "FailedCreatePodSandBox"));
        assert_eq!(detection.map(Detection::id), Some("D010"));
        let detection = detection.expect("a detection");
        // An event, settled like D006 and D007, and no log to read: no container ran.
        assert!(detection.settles());
        assert!(!detection.is_state());
        assert!(!reads_logs(detection));
        // Only about a Pod.
        assert_eq!(detect_event(&event("Node", "FailedCreatePodSandBox")), None);
        assert_eq!(
            detect_event(&event("Deployment", "FailedCreatePodSandBox")),
            None
        );
    }
}
