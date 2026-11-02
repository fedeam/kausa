//! The detection rules: which container states, Warning events and Deployment `Progressing`
//! conditions are failures kausa explains.
//!
//! The catalogue, with ids and triggers, is docs/reference/rules.md. Rules are pure functions of a
//! record: no I/O, no clock, no state.

use crate::record::{ContainerRecord, DeploymentRecord, EventRecord, PodRecord};

/// A detection rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Detection {
    /// `D001`: the container is waiting in `CrashLoopBackOff`.
    CrashLoop,
    /// `D002`: the image cannot be pulled (or, with `imagePullPolicy: Never`, cannot be used).
    ImagePull,
    /// `D003`: the container was killed for exceeding its memory limit.
    OutOfMemory,
    /// `D004`: the container cannot be created (a missing Secret or ConfigMap key, for example) or
    /// its process cannot be started (a missing or non-executable entrypoint).
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
    /// `D011`: a workload controller cannot create its Pods, or a CronJob its Jobs, because the API
    /// server refuses them (a quota, an admission webhook or Pod Security admission).
    PodCreate,
    /// `D012`: a PersistentVolumeClaim cannot be provisioned (a StorageClass that does not exist,
    /// or a request the provisioner refuses).
    ClaimProvisioning,
    /// `D013`: a HorizontalPodAutoscaler cannot get its metrics, compute a replica count from
    /// them, or read its target's scale (no metrics API, a target that does not exist).
    AutoscalerMetrics,
    /// `D014`: a Deployment's rollout passed its progress deadline (`Progressing` is `False` with
    /// reason `ProgressDeadlineExceeded`). It yields to a Pod or event incident of the same
    /// Deployment (docs/adr/0018-stuck-rollouts.md). The last variant, so it sorts last.
    RolloutStalled,
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
            Detection::RolloutStalled => "D014",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Detection::CrashLoop => "crash loop",
            Detection::ImagePull => "image cannot be pulled",
            Detection::OutOfMemory => "out of memory",
            Detection::ContainerCreate => "container cannot be created or started",
            Detection::Unschedulable => "pod cannot be scheduled",
            Detection::ProbeFailure => "probe failures",
            Detection::VolumeFailure => "volume cannot be attached or mounted",
            Detection::JobFailed => "job failed",
            Detection::PodEvicted => "pod evicted",
            Detection::SandboxCreate => "pod sandbox cannot be created",
            Detection::PodCreate => "workload cannot create pods or jobs",
            Detection::ClaimProvisioning => "volume claim cannot be provisioned",
            Detection::AutoscalerMetrics => "autoscaler cannot get metrics",
            Detection::RolloutStalled => "rollout does not progress",
        }
    }

    /// Whether the failure has to persist through the settle window before it is an incident. A
    /// failed Job is final, so it is an incident at once.
    pub fn settles(self) -> bool {
        self != Detection::JobFailed
    }

    /// Whether the signal is a state that kausa reads at every check (from the Pod: container
    /// states, the Pod's scheduling condition, its eviction; from the Deployment: its `Progressing`
    /// condition), rather than an event.
    pub fn is_state(self) -> bool {
        match self {
            Detection::CrashLoop => true,
            Detection::ImagePull => true,
            Detection::OutOfMemory => true,
            Detection::ContainerCreate => true,
            Detection::Unschedulable => true,
            Detection::ProbeFailure => false,
            Detection::VolumeFailure => false,
            Detection::JobFailed => false,
            Detection::PodEvicted => true,
            Detection::SandboxCreate => false,
            Detection::PodCreate => false,
            Detection::ClaimProvisioning => false,
            Detection::AutoscalerMetrics => false,
            Detection::RolloutStalled => true,
        }
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

/// Whether the Deployment's rollout passed its progress deadline: its `Progressing` condition is
/// `False` with reason `ProgressDeadlineExceeded` (exact comparisons), it is not paused, and its
/// `status.observedGeneration` is at least its `metadata.generation`, both present. The deployment
/// controller emits no Event for the timeout; the condition is the only signal. It keeps the
/// reason on a Deployment paused after the timeout (`checkPausedConditions`), which waits on
/// purpose, and a condition older than the last edit of the spec does not count (deployment
/// controller source, pkg/controller/deployment, read on 2026-10-29).
pub fn detect_rollout(d: &DeploymentRecord) -> bool {
    d.progressing_status.as_deref() == Some("False")
        && d.progressing_reason.as_deref() == Some("ProgressDeadlineExceeded")
        && !d.paused
        && matches!(
            (d.generation, d.observed_generation),
            (Some(generation), Some(observed)) if observed >= generation
        )
}

/// The detection that a container's state matches, if any. A container matches at most one: an
/// out-of-memory kill takes precedence over the crash loop it causes, and so does a process the
/// runtime could not start (last termination `StartError`), which is `D004`.
pub fn detect_container(container: &ContainerRecord) -> Option<Detection> {
    let oom = |reason: &Option<String>| reason.as_deref() == Some("OOMKilled");
    match container.waiting_reason.as_deref() {
        Some("CrashLoopBackOff") if oom(&container.last_termination_reason) => {
            Some(Detection::OutOfMemory)
        }
        // A container whose process the runtime cannot start (a missing or non-executable
        // entrypoint) waits in `RunContainerError` (below) most of the time and in
        // `CrashLoopBackOff` between restarts, its last termination `StartError` either way (seen
        // on kind, Kubernetes v1.36.4, on 2026-11-02: `RunContainerError` in 114 of 126 samples
        // over 6 minutes; why it dominates is a hypothesis, not traced). The back-off is the same
        // failure, so it stays `D004`: one incident, and no log of a container that never ran.
        // The back-off, with its `BackOff` event:
        // pkg/kubelet/kuberuntime/kuberuntime_manager.go:1934-1961 at v1.36.4.
        Some("CrashLoopBackOff")
            if container.last_termination_reason.as_deref() == Some("StartError") =>
        {
            Some(Detection::ContainerCreate)
        }
        Some("CrashLoopBackOff") => Some(Detection::CrashLoop),
        // `ErrImageNeverPull`: `imagePullPolicy: Never` and an image the node cannot use. The
        // kubelet returns the same reason whether the image is absent or present but not usable
        // with the Pod's credentials (pkg/kubelet/images/types.go:38 and image_manager.go:150-159
        // at v1.36.4), so it does not say the image is missing.
        Some("ErrImagePull" | "ImagePullBackOff" | "InvalidImageName" | "ErrImageNeverPull") => {
            Some(Detection::ImagePull)
        }
        // `RunContainerError`: the runtime created the container but cannot start its process
        // (pkg/kubelet/container/sync_result.go:87 at v1.36.4). The kubelet writes a Warning
        // `Failed` event `Error: <runtime message>` on the Pod
        // (pkg/kubelet/kuberuntime/kuberuntime_container.go:292-296), and the waiting reason it
        // keeps in its reason cache overrides the terminated state
        // (pkg/kubelet/kubelet_pods.go:2731-2748), so `state.terminated` never appears and the
        // restart-count path below never sees it.
        Some("CreateContainerConfigError" | "CreateContainerError" | "RunContainerError") => {
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
        // The Job controller's `recordJobFinished` writes the reason of the Job's `Failed`
        // condition as a Warning event on the Job (pkg/controller/job/job_controller.go:1666 at
        // v1.36.4). The reason is one of these five (pkg/apis/batch/types.go:622-633):
        // `PodFailurePolicy` when a Pod matches a `FailJob` rule of its `podFailurePolicy`
        // (:1081), `MaxFailedIndexesExceeded` and `FailedIndexes` for an indexed Job with a
        // per-index backoff limit (:1107, :1109). Exact, case-sensitive comparisons; the interim
        // `FailureTarget` condition writes no event (seen on kind, Kubernetes v1.36.4, on
        // 2026-11-02).
        (
            "Job",
            "BackoffLimitExceeded"
            | "DeadlineExceeded"
            | "PodFailurePolicy"
            | "MaxFailedIndexesExceeded"
            | "FailedIndexes",
        ) => Some(Detection::JobFailed),
        // The ReplicaSet, Job and DaemonSet controllers (through the shared Pod control,
        // `FailedCreatePodReason` in pkg/controller/controller_utils.go) and the StatefulSet
        // controller (pkg/controller/statefulset/stateful_pod_control.go) emit it on themselves
        // when the API server refuses a Pod; no Pod exists to be about (seen on kind, Kubernetes
        // 1.37, with a ResourceQuota and Pod Security admission, on 2026-10-28). The CronJob
        // controller emits it on the CronJob when the API server refuses the Job of a scheduled
        // time (pkg/controller/cronjob/cronjob_controllerv2.go:640 at v1.36.4); no Job exists to
        // be about (seen on kind, Kubernetes v1.36.4, with a ResourceQuota `count/jobs.batch: "0"`,
        // on 2026-11-02).
        ("ReplicaSet" | "Job" | "StatefulSet" | "DaemonSet" | "CronJob", "FailedCreate") => {
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

    #[test]
    fn all_lists_every_detection() {
        // A new variant breaks this match, a reminder to add it to `ALL` too.
        for detection in ALL {
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
        // `RolloutStalled` is the last variant, so a variant missing from `ALL` shifts its
        // discriminant.
        let discriminants: Vec<_> = ALL.iter().map(|&d| d as usize).collect();
        assert_eq!(
            discriminants,
            (0..=Detection::RolloutStalled as usize).collect::<Vec<_>>()
        );
        // A variant after `RolloutStalled` shifts nothing, but `Detection::id` gives it an id: the
        // ids of `ALL` are the string literals `"D…"` before the tests.
        let source = include_str!("detect.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        let mut literals: Vec<_> = code
            .split('"')
            .filter(|s| {
                s.len() == 4 && s.starts_with('D') && s[1..].bytes().all(|b| b.is_ascii_digit())
            })
            .collect();
        literals.sort();
        let ids: Vec<_> = ALL.iter().map(|d| d.id()).collect();
        assert_eq!(literals, ids);
    }

    #[test]
    fn ids_are_unique_and_in_order() {
        let ids: Vec<_> = ALL.iter().map(|d| d.id()).collect();
        assert_eq!(
            ids,
            [
                "D001", "D002", "D003", "D004", "D005", "D006", "D007", "D008", "D009", "D010",
                "D011", "D012", "D013", "D014"
            ]
        );
        // The last variant sorts last, so the tracker's second pass sees every other key.
        let mut sorted = ALL;
        sorted.sort();
        assert_eq!(sorted, ALL);
    }

    #[test]
    fn every_id_is_in_the_rules_reference() {
        // The id in backquotes at the start of each row of the detection table, in order.
        let documented: Vec<_> = include_str!("../docs/reference/rules.md")
            .lines()
            .filter_map(|line| line.strip_prefix("| `")?.split_once('`'))
            .map(|(id, _)| id)
            .filter(|id| id.starts_with('D'))
            .collect();
        let ids: Vec<_> = ALL.iter().map(|d| d.id()).collect();
        assert_eq!(documented, ids);
    }

    #[test]
    fn container_states() {
        assert_eq!(
            detect_container(&waiting("CrashLoopBackOff")),
            Some(Detection::CrashLoop)
        );
        for reason in [
            "ErrImagePull",
            "ImagePullBackOff",
            "InvalidImageName",
            "ErrImageNeverPull",
        ] {
            assert_eq!(
                detect_container(&waiting(reason)),
                Some(Detection::ImagePull),
                "{reason}"
            );
        }
        for reason in [
            "CreateContainerConfigError",
            "CreateContainerError",
            "RunContainerError",
        ] {
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
        // The CronJob controller writes it on the CronJob when the API server refuses its Job.
        for kind in ["ReplicaSet", "Job", "StatefulSet", "DaemonSet", "CronJob"] {
            let detection = detect_event(&event(kind, "FailedCreate"));
            assert_eq!(detection.map(Detection::id), Some("D011"), "{kind}");
            let detection = detection.expect("a detection");
            assert_eq!(detection.name(), "workload cannot create pods or jobs");
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

    // Acceptance tests for the coverage gaps of v0.5 (no ADR), written before the code.
    // Every case was reproduced on kind (Kubernetes v1.36.4) on 2026-11-02; the reasons are the
    // upstream constants at v1.36.4 (pkg/apis/batch/types.go:622-633,
    // pkg/controller/cronjob/cronjob_controllerv2.go:640, pkg/kubelet/images/types.go:38,
    // pkg/kubelet/container/sync_result.go:87).

    #[test]
    fn every_failed_condition_reason_of_a_job_is_d008() {
        // `recordJobFinished` writes the reason of the Job's `Failed` condition as a Warning on the
        // Job, whichever of the five it is.
        for reason in [
            "BackoffLimitExceeded",
            "DeadlineExceeded",
            "PodFailurePolicy",
            "MaxFailedIndexesExceeded",
            "FailedIndexes",
        ] {
            let detection = detect_event(&event("Job", reason));
            assert_eq!(detection.map(Detection::id), Some("D008"), "{reason}");
            let detection = detection.expect("a detection");
            assert_eq!(detection.name(), "job failed");
            // A failed Job is final, an event, and its first failed Pod's log is read.
            assert!(!detection.settles(), "{reason}");
            assert!(!detection.is_state(), "{reason}");
            assert!(reads_logs(detection), "{reason}");
        }
        // Not D008: the Job's other reasons (a success, the interim `FailureTarget` condition), a
        // reason in another case, and the same reasons about another kind.
        for (kind, reason) in [
            ("Job", "TooManySucceededPods"),
            ("Job", "Completed"),
            ("Job", "FailureTarget"),
            ("Job", "podfailurepolicy"),
            ("Job", "failedindexes"),
            ("CronJob", "PodFailurePolicy"),
            ("CronJob", "BackoffLimitExceeded"),
            ("Pod", "FailedIndexes"),
            ("Pod", "PodFailurePolicy"),
        ] {
            assert_eq!(detect_event(&event(kind, reason)), None, "{kind} {reason}");
        }
    }

    #[test]
    fn a_cronjob_that_cannot_create_its_job_is_d011() {
        let detection = detect_event(&event("CronJob", "FailedCreate"));
        assert_eq!(detection.map(Detection::id), Some("D011"));
        let detection = detection.expect("a detection");
        assert_eq!(detection.name(), "workload cannot create pods or jobs");
        settled_event_without_logs(detection);
        // Not D011: the CronJob controller's other Warning reasons, and the reason in another
        // case.
        for (kind, reason) in [
            ("CronJob", "FailedDelete"),
            ("CronJob", "UnexpectedJob"),
            ("CronJob", "failedcreate"),
            ("Job", "failedcreate"),
        ] {
            assert_eq!(detect_event(&event(kind, reason)), None, "{kind} {reason}");
        }
    }

    #[test]
    fn an_image_the_policy_never_pulls_is_d002() {
        // `ErrImageNeverPull`: `imagePullPolicy: Never` and an image the node cannot use, absent
        // or present but not usable with the Pod's credentials (image_manager.go:150-159).
        let detection = detect_container(&waiting("ErrImageNeverPull"));
        assert_eq!(detection, Some(Detection::ImagePull));
        let detection = detection.expect("a detection");
        assert_eq!(detection.name(), "image cannot be pulled");
        assert!(detection.settles());
        assert!(detection.is_state());
        assert!(!reads_logs(detection));
        // Exact: another case is no reason the kubelet writes.
        assert_eq!(detect_container(&waiting("errimageneverpull")), None);
    }

    /// A container waiting in `reason` whose last termination was `last`.
    fn waiting_after(reason: &str, last: &str) -> ContainerRecord {
        ContainerRecord {
            last_termination_reason: Some(last.into()),
            last_termination_exit_code: Some(128),
            restart_count: 4,
            ..waiting(reason)
        }
    }

    #[test]
    fn a_container_that_cannot_start_is_d004() {
        // The runtime cannot start the container's process (a missing or non-executable
        // entrypoint): the kubelet reports `RunContainerError` most of the time and
        // `CrashLoopBackOff` in between, the last termination `StartError` either way.
        for container in [
            waiting("RunContainerError"),
            waiting_after("RunContainerError", "StartError"),
            waiting_after("CrashLoopBackOff", "StartError"),
        ] {
            let detection = detect_container(&container);
            assert_eq!(detection, Some(Detection::ContainerCreate), "{container:?}");
        }
        let detection = Detection::ContainerCreate;
        assert_eq!(detection.id(), "D004");
        assert_eq!(detection.name(), "container cannot be created or started");
        assert!(detection.settles());
        assert!(detection.is_state());
        // No log: the container never ran.
        assert!(!reads_logs(detection));
    }

    #[test]
    fn only_a_start_error_takes_the_crash_loop_to_d004() {
        // The precedence of `StartError` sits with the out-of-memory kill's; any other last
        // termination keeps the crash loop.
        assert_eq!(
            detect_container(&waiting_after("CrashLoopBackOff", "OOMKilled")),
            Some(Detection::OutOfMemory)
        );
        for last in ["Error", "Completed", "ContainerCannotRun", "startError"] {
            assert_eq!(
                detect_container(&waiting_after("CrashLoopBackOff", last)),
                Some(Detection::CrashLoop),
                "{last}"
            );
        }
        assert_eq!(
            detect_container(&waiting("CrashLoopBackOff")),
            Some(Detection::CrashLoop)
        );
        // Not changed (never observed: `state.terminated` did not appear in any sample): a
        // terminated `StartError` after restarts stays on the restart-count path, a crash loop.
        let terminated = ContainerRecord {
            terminated_reason: Some("StartError".into()),
            terminated_exit_code: Some(128),
            restart_count: 3,
            ..Default::default()
        };
        assert_eq!(detect_container(&terminated), Some(Detection::CrashLoop));
    }

    #[test]
    fn a_container_that_cannot_start_stays_d004_in_a_failed_pod() {
        // Unlike a crash loop, which a Pod in phase `Failed` never runs again.
        let pod = PodRecord {
            phase: Some("Failed".into()),
            containers: vec![
                waiting("RunContainerError"),
                waiting_after("CrashLoopBackOff", "StartError"),
            ],
            ..Default::default()
        };
        for container in &pod.containers {
            assert_eq!(
                detect_pod_container(&pod, container),
                Some(Detection::ContainerCreate),
                "{container:?}"
            );
        }
        assert_eq!(
            detect_pod_container(&pod, &waiting_after("CrashLoopBackOff", "Error")),
            None
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

    // Acceptance tests for `D014` (docs/adr/0018-stuck-rollouts.md), written before the code: a
    // Deployment whose `Progressing` condition says that its rollout passed its progress deadline.
    // The condition values are the deployment controller's (pkg/controller/deployment, read on
    // 2026-10-29); names are invented.

    /// A Deployment record with the `Progressing` condition `(status, reason)`, generation 3 and
    /// observed generation 3, not paused.
    fn rollout(condition: Option<(&str, &str)>) -> crate::record::DeploymentRecord {
        crate::record::DeploymentRecord {
            namespace: "shop".into(),
            name: "checkout".into(),
            uid: "uid-checkout".into(),
            generation: Some(3),
            observed_generation: Some(3),
            paused: false,
            revision: Some(7),
            replicas: Some(3),
            progress_deadline_seconds: Some(600),
            min_ready_seconds: Some(0),
            updated_replicas: 1,
            ready_replicas: 2,
            available_replicas: 2,
            unavailable_replicas: 1,
            progressing_status: condition.map(|(status, _)| status.to_owned()),
            progressing_reason: condition.map(|(_, reason)| reason.to_owned()),
        }
    }

    const TIMED_OUT: Option<(&str, &str)> = Some(("False", "ProgressDeadlineExceeded"));

    #[test]
    fn a_rollout_past_its_progress_deadline_is_d014() {
        assert!(detect_rollout(&rollout(TIMED_OUT)));
        // A status written for a newer generation than the spec's still counts.
        let mut ahead = rollout(TIMED_OUT);
        ahead.observed_generation = Some(4);
        assert!(detect_rollout(&ahead));

        let detection = Detection::RolloutStalled;
        assert_eq!(detection.id(), "D014");
        assert_eq!(detection.name(), "rollout does not progress");
        assert!(detection.settles());
        assert!(detection.is_state());
        assert!(!reads_logs(detection));
    }

    #[test]
    fn other_progressing_conditions_are_not_d014() {
        for condition in [
            ("True", "ReplicaSetUpdated"),
            ("True", "NewReplicaSetAvailable"),
            ("True", "NewReplicaSetCreated"),
            ("True", "FoundNewReplicaSet"),
            ("Unknown", "DeploymentPaused"),
            ("Unknown", "DeploymentResumed"),
            ("False", "ReplicaSetCreateError"),
            // The comparisons are exact.
            ("false", "progressdeadlineexceeded"),
            ("False", "progressdeadlineexceeded"),
            ("false", "ProgressDeadlineExceeded"),
            ("False", "ProgressDeadlineExceeded "),
            ("True", "ProgressDeadlineExceeded"),
            ("Unknown", "ProgressDeadlineExceeded"),
        ] {
            assert!(!detect_rollout(&rollout(Some(condition))), "{condition:?}");
        }
        // No deadline: the controller removes the condition.
        assert!(!detect_rollout(&rollout(None)));
    }

    #[test]
    fn a_paused_or_unobserved_rollout_is_not_d014() {
        // `checkPausedConditions` keeps `ProgressDeadlineExceeded` on a Deployment paused after
        // its timeout: a paused Deployment waits on purpose.
        let mut paused = rollout(TIMED_OUT);
        paused.paused = true;
        assert!(!detect_rollout(&paused));
        // The condition is older than the last edit of the spec.
        let mut stale = rollout(TIMED_OUT);
        stale.observed_generation = Some(2);
        assert!(!detect_rollout(&stale));
        let mut unobserved = rollout(TIMED_OUT);
        unobserved.observed_generation = None;
        assert!(!detect_rollout(&unobserved));
        let mut no_generation = rollout(TIMED_OUT);
        no_generation.generation = None;
        assert!(!detect_rollout(&no_generation));
        let mut neither = rollout(TIMED_OUT);
        (neither.generation, neither.observed_generation) = (None, None);
        assert!(!detect_rollout(&neither));
    }

    #[test]
    fn the_counts_and_the_revision_play_no_part_in_d014() {
        let mut healthy_counts = rollout(TIMED_OUT);
        (
            healthy_counts.updated_replicas,
            healthy_counts.ready_replicas,
            healthy_counts.available_replicas,
            healthy_counts.unavailable_replicas,
        ) = (3, 3, 3, 0);
        healthy_counts.revision = None;
        healthy_counts.progress_deadline_seconds = None;
        healthy_counts.replicas = None;
        assert!(detect_rollout(&healthy_counts));
    }
}
