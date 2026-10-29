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
    /// `D009`: the kubelet evicted the Pod (its ephemeral-storage limit, an `emptyDir` size limit,
    /// or node pressure).
    PodEvicted,
    /// `D010`: the runtime cannot create the Pod's sandbox (a missing `RuntimeClass` handler, or a
    /// network plugin that fails to set up the Pod's network).
    SandboxCreate,
    /// `D011`: a workload controller cannot create its Pods, because the API server refuses them
    /// (a quota, an admission webhook or Pod Security admission).
    PodCreate,
    /// `D012`: a PersistentVolumeClaim cannot be provisioned (a StorageClass that does not exist,
    /// or a request the provisioner refuses).
    ClaimProvisioning,
    /// `D013`: a HorizontalPodAutoscaler cannot get its metrics, compute a replica count from
    /// them, or read its target's scale (no metrics API, a target that does not exist).
    AutoscalerMetrics,
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
            Detection::PodEvicted => "D009",
            Detection::SandboxCreate => "D010",
            Detection::PodCreate => "D011",
            Detection::ClaimProvisioning => "D012",
            Detection::AutoscalerMetrics => "D013",
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
            Detection::PodEvicted => "pod evicted",
            Detection::SandboxCreate => "pod sandbox cannot be created",
            Detection::PodCreate => "workload cannot create pods",
            Detection::ClaimProvisioning => "volume claim cannot be provisioned",
            Detection::AutoscalerMetrics => "autoscaler cannot get metrics",
        }
    }

    /// Whether the failure has to persist through the settle window before it is an incident. A
    /// failed Job is final, so it is an incident at once.
    pub fn settles(self) -> bool {
        self != Detection::JobFailed
    }

    /// Whether the signal is a state that kausa reads from the Pod at every check (container
    /// states, the Pod's scheduling condition, its eviction), rather than an event.
    pub fn is_state(self) -> bool {
        matches!(
            self,
            Detection::CrashLoop
                | Detection::ImagePull
                | Detection::OutOfMemory
                | Detection::ContainerCreate
                | Detection::Unschedulable
                | Detection::PodEvicted
        )
    }
}

/// Whether the Pod cannot be scheduled: still `Pending`, with its `PodScheduled` condition `False`
/// for reason `Unschedulable`, or `SchedulerError` when a scheduler plugin failed for the Pod (a
/// volume the `VolumeBinding` plugin cannot bind in `PreBind`, seen on kind, Kubernetes 1.37, on
/// 2026-10-28). A Pod held by its scheduling gates (`SchedulingGated`) waits on purpose. The
/// scheduler's `FailedScheduling` events say the same, but they may stop being updated for a Pod
/// that keeps failing, while the condition holds as long as the failure: the condition is the
/// signal.
pub fn detect_unschedulable(pod: &PodRecord) -> bool {
    pod.phase.as_deref() == Some("Pending") && pod.unschedulable.is_some()
}

/// Whether the kubelet evicted the Pod: phase `Failed` with `status.reason` `Evicted`. The kubelet
/// writes the same reason when it refuses a new Pod on a node under pressure (a message starting
/// with `Pod was rejected:`); that is a consequence of the node's state, not of the workload, and
/// is not an eviction (seen on kind, Kubernetes 1.37, on 2026-10-28). How recent the eviction is,
/// is checked against the clock in `collect_signals`.
pub fn detect_evicted(pod: &PodRecord) -> bool {
    pod.phase.as_deref() == Some("Failed")
        && pod.reason.as_deref() == Some("Evicted")
        && !pod
            .message
            .as_deref()
            .is_some_and(|message| message.starts_with("Pod was rejected:"))
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

/// The detection that a container of `pod` matches, if any: `detect_container`, except that no
/// container of a Pod in phase `Failed` is a crash loop. Such a Pod runs none of its containers
/// again; a container that had restarted before the kubelet evicted the Pod or ended it at its
/// `activeDeadlineSeconds` is left terminated (`Error`, exit code 137) after a restart, which is
/// not a crash loop (seen on kind, Kubernetes 1.37, on 2026-10-28). An out-of-memory kill stays
/// `D003`.
pub fn detect_pod_container(pod: &PodRecord, container: &ContainerRecord) -> Option<Detection> {
    match detect_container(container) {
        Some(Detection::CrashLoop) if pod.phase.as_deref() == Some("Failed") => None,
        detection => detection,
    }
}

/// The detection that a Warning event matches, if any. Events about Pods, about the controllers of
/// their workloads, about PersistentVolumeClaims and about HorizontalPodAutoscalers; scheduling
/// failures come from the Pod's condition instead (`detect_unschedulable`).
pub fn detect_event(event: &EventRecord) -> Option<Detection> {
    match (event.regarding.kind.as_str(), event.reason.as_str()) {
        ("Pod", "Unhealthy") => Some(Detection::ProbeFailure),
        ("Pod", "FailedMount" | "FailedAttachVolume") => Some(Detection::VolumeFailure),
        // The kubelet's event when the runtime cannot create the Pod's sandbox; the Pod waits in
        // `ContainerCreating`, which is no failure state (seen on kind, Kubernetes 1.37, with a
        // `RuntimeClass` whose handler the nodes lack, on 2026-10-28).
        ("Pod", "FailedCreatePodSandBox") => Some(Detection::SandboxCreate),
        // The Job controller emits these as Warning events on the Job when it marks it failed
        // (pkg/controller/job/job_controller.go, checked on 2026-10-26).
        ("Job", "BackoffLimitExceeded" | "DeadlineExceeded") => Some(Detection::JobFailed),
        // The ReplicaSet, Job and DaemonSet controllers (through the shared Pod control,
        // `FailedCreatePodReason` in pkg/controller/controller_utils.go) and the StatefulSet
        // controller (pkg/controller/statefulset/stateful_pod_control.go) emit it on themselves
        // when the API server refuses a Pod; no Pod exists to be about (seen on kind, Kubernetes
        // 1.37, with a ResourceQuota and Pod Security admission, on 2026-10-28).
        ("ReplicaSet" | "Job" | "StatefulSet" | "DaemonSet", "FailedCreate") => {
            Some(Detection::PodCreate)
        }
        // The PersistentVolume controller (`provisionClaim` in
        // pkg/controller/volume/persistentvolume/pv_controller.go) and external provisioners
        // (sig-storage-lib-external-provisioner) emit it on the claim when they cannot provision
        // its volume (seen on kind, Kubernetes 1.37, with a StorageClass that does not exist and
        // with an access mode the provisioner refuses, on 2026-10-28).
        ("PersistentVolumeClaim", "ProvisioningFailed") => Some(Detection::ClaimProvisioning),
        // The HorizontalPodAutoscaler controller (pkg/controller/podautoscaler/horizontal.go)
        // emits these on the autoscaler when it cannot read a metric, compute a replica count from
        // the metrics, or read its target's scale; a failed rescale after that is not one of them
        // (seen on kind, Kubernetes 1.37, with no metrics API and with a target that does not
        // exist, on 2026-10-28).
        (
            "HorizontalPodAutoscaler",
            "FailedGetResourceMetric"
            | "FailedGetContainerResourceMetric"
            | "FailedGetPodsMetric"
            | "FailedGetObjectMetric"
            | "FailedGetExternalMetric"
            | "FailedComputeMetricsReplicas"
            | "FailedGetScale",
        ) => Some(Detection::AutoscalerMetrics),
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
            Detection::PodEvicted,
            Detection::SandboxCreate,
            Detection::PodCreate,
            Detection::ClaimProvisioning,
            Detection::AutoscalerMetrics,
        ];
        let ids: Vec<_> = all.iter().map(|d| d.id()).collect();
        assert_eq!(
            ids,
            [
                "D001", "D002", "D003", "D004", "D005", "D006", "D007", "D008", "D009", "D010",
                "D011", "D012", "D013"
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
    fn no_container_of_a_failed_pod_is_a_crash_loop() {
        let restarted = ContainerRecord {
            terminated_reason: Some("Error".into()),
            terminated_exit_code: Some(137),
            restart_count: 1,
            ..Default::default()
        };
        let pod = |phase: &str, containers: Vec<ContainerRecord>| PodRecord {
            phase: Some(phase.into()),
            containers,
            ..Default::default()
        };
        let failed = pod(
            "Failed",
            vec![restarted.clone(), waiting("CrashLoopBackOff")],
        );
        for container in &failed.containers {
            assert_eq!(detect_pod_container(&failed, container), None);
        }
        // The same states in a Pod that still runs, and an out-of-memory kill in a failed Pod.
        let running = pod("Running", vec![restarted.clone()]);
        assert_eq!(
            detect_pod_container(&running, &running.containers[0]),
            Some(Detection::CrashLoop)
        );
        let oom = ContainerRecord {
            terminated_reason: Some("OOMKilled".into()),
            ..restarted
        };
        assert_eq!(
            detect_pod_container(&failed, &oom),
            Some(Detection::OutOfMemory)
        );
    }

    #[test]
    fn an_evicted_pod_is_d009() {
        let pod = |phase: &str, reason: &str, message: &str| PodRecord {
            phase: Some(phase.into()),
            reason: Some(reason.into()),
            message: Some(message.into()),
            ..Default::default()
        };
        for message in [
            "Pod ephemeral local storage usage exceeds the total limit of containers 50Mi. ",
            "Usage of EmptyDir volume \"scratch\" exceeds the limit \"20Mi\". ",
            "The node was low on resource: ephemeral-storage. ",
        ] {
            assert!(
                detect_evicted(&pod("Failed", "Evicted", message)),
                "{message}"
            );
        }
        // The kubelet refusing a new Pod on a node under pressure, a Pod past its deadline, and
        // a Pod that has not failed (yet).
        let rejected = "Pod was rejected: The node had condition: [DiskPressure]. ";
        assert!(!detect_evicted(&pod("Failed", "Evicted", rejected)));
        let deadline = "Pod was active on the node longer than the specified deadline";
        assert!(!detect_evicted(&pod(
            "Failed",
            "DeadlineExceeded",
            deadline
        )));
        assert!(!detect_evicted(&pod("Running", "Evicted", "")));
        assert!(!detect_evicted(&PodRecord {
            phase: Some("Failed".into()),
            ..Default::default()
        }));
        assert!(Detection::PodEvicted.is_state());
        assert!(Detection::PodEvicted.settles());
        assert!(!reads_logs(Detection::PodEvicted));
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

    // Acceptance tests for `D011`, `D012` and `D013`, written before the code: rules from Warning
    // events about an object that is not a Pod (a workload controller, a claim, an autoscaler),
    // reproduced on kind (Kubernetes 1.37) on 2026-10-28. They name the detections by id, so the
    // variants are free to choose; `ids_are_unique_and_in_order` gets the three new ones.

    /// Asserts that `detection` is an event rule settled like `D006` and `D007`, that reads no log.
    fn settled_event_without_logs(detection: Detection) {
        assert!(detection.settles(), "{detection:?}");
        assert!(!detection.is_state(), "{detection:?}");
        assert!(!reads_logs(detection), "{detection:?}");
    }

    #[test]
    fn a_controller_that_cannot_create_pods_is_d011() {
        // The ReplicaSet, Job and DaemonSet controllers write `FailedCreate` on themselves when
        // the API server refuses a Pod (a quota, an admission webhook, Pod Security admission),
        // and the StatefulSet controller writes it on the StatefulSet; no Pod exists to be about.
        for kind in ["ReplicaSet", "Job", "StatefulSet", "DaemonSet"] {
            let detection = detect_event(&event(kind, "FailedCreate"));
            assert_eq!(detection.map(Detection::id), Some("D011"), "{kind}");
            let detection = detection.expect("a detection");
            assert_eq!(detection.name(), "workload cannot create pods");
            settled_event_without_logs(detection);
        }
        // Not D011: the same reason about a Pod or a Deployment (its ReplicaSet reports the
        // failure), the controllers' other failures, and a Job's own failure, which stays D008.
        assert_eq!(detect_event(&event("Pod", "FailedCreate")), None);
        assert_eq!(detect_event(&event("Deployment", "FailedCreate")), None);
        assert_eq!(detect_event(&event("ReplicaSet", "FailedDelete")), None);
        assert_eq!(detect_event(&event("StatefulSet", "FailedDelete")), None);
        assert_eq!(
            detect_event(&event("Job", "BackoffLimitExceeded")).map(Detection::id),
            Some("D008")
        );
    }

    #[test]
    fn a_claim_that_cannot_be_provisioned_is_d012() {
        // The PersistentVolume controller (a StorageClass that does not exist) and external
        // provisioners (a request they refuse) write `ProvisioningFailed` on the claim.
        let detection = detect_event(&event("PersistentVolumeClaim", "ProvisioningFailed"));
        assert_eq!(detection.map(Detection::id), Some("D012"));
        let detection = detection.expect("a detection");
        assert_eq!(detection.name(), "volume claim cannot be provisioned");
        settled_event_without_logs(detection);
        // Not D012: waiting for an external provisioner (a Normal event, but the reason alone
        // must not match either), the same reason about another kind, and a mount failure (D007
        // only about a Pod).
        for (kind, reason) in [
            ("PersistentVolumeClaim", "ExternalProvisioning"),
            ("Pod", "ProvisioningFailed"),
            ("PersistentVolume", "ProvisioningFailed"),
            ("PersistentVolumeClaim", "FailedMount"),
        ] {
            assert_eq!(detect_event(&event(kind, reason)), None, "{kind} {reason}");
        }
    }

    #[test]
    fn an_autoscaler_that_cannot_get_metrics_is_d013() {
        // Every Warning reason the HorizontalPodAutoscaler controller writes on the autoscaler
        // when it cannot read a metric, compute a replica count from them, or read its target's
        // scale (pkg/controller/podautoscaler/horizontal.go, Kubernetes v1.37.0).
        for reason in [
            "FailedGetResourceMetric",
            "FailedGetContainerResourceMetric",
            "FailedGetPodsMetric",
            "FailedGetObjectMetric",
            "FailedGetExternalMetric",
            "FailedComputeMetricsReplicas",
            "FailedGetScale",
        ] {
            let detection = detect_event(&event("HorizontalPodAutoscaler", reason));
            assert_eq!(detection.map(Detection::id), Some("D013"), "{reason}");
            let detection = detection.expect("a detection");
            assert_eq!(detection.name(), "autoscaler cannot get metrics");
            settled_event_without_logs(detection);
        }
        // Not D013: a rescale that failed after the metrics were read, a successful one, and the
        // same reasons about another kind.
        for (kind, reason) in [
            ("HorizontalPodAutoscaler", "FailedRescale"),
            ("HorizontalPodAutoscaler", "SuccessfulRescale"),
            ("Deployment", "FailedGetScale"),
            ("Pod", "FailedGetResourceMetric"),
        ] {
            assert_eq!(detect_event(&event(kind, reason)), None, "{kind} {reason}");
        }
    }
}
