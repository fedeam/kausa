//! The small records kausa keeps instead of whole objects.
//!
//! A watch delivers whole Pods and Events, inline environment values and command lines included.
//! Each one is reduced here, at once, to the few fields the detection rules and (later) the request
//! to the model need; the object itself is dropped and never logged
//! (docs/explanation/architecture.md, "Watching without a large cache").

use k8s_openapi::api::core::v1::{Container, ContainerStatus, ObjectReference, Pod, Probe};
use k8s_openapi::api::events::v1::Event;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use k8s_openapi::jiff::Timestamp;

/// The longest message or note kept from the cluster, in bytes. Longer text is cut at a character
/// boundary, so a hostile workload cannot grow kausa's memory through one field.
pub const MAX_TEXT_BYTES: usize = 1024;

/// `text` cut to at most `MAX_TEXT_BYTES` bytes, at a UTF-8 character boundary.
pub fn bounded(text: &str) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A reference to an owner, taken from an owner reference or an event's `regarding`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnerRef {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub uid: String,
}

impl OwnerRef {
    fn from_owner_reference(owner: &OwnerReference) -> Self {
        Self {
            api_version: owner.api_version.clone(),
            kind: owner.kind.clone(),
            name: owner.name.clone(),
            uid: owner.uid.clone(),
        }
    }
}

/// The controller owner of an object, if it has one: the owner reference with `controller: true`.
pub fn controller_of(owners: Option<&Vec<OwnerReference>>) -> Option<OwnerRef> {
    owners?
        .iter()
        .find(|owner| owner.controller == Some(true))
        .map(OwnerRef::from_owner_reference)
}

/// What kausa keeps of a Pod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodRecord {
    pub namespace: String,
    pub name: String,
    pub uid: String,
    /// The controller owner reference, if any (a ReplicaSet, StatefulSet, DaemonSet, Job or other).
    pub controller: Option<OwnerRef>,
    pub phase: Option<String>,
    /// The message of the `PodScheduled` condition when the scheduler cannot place the Pod
    /// (status `False`, reason `Unschedulable`).
    pub unschedulable: Option<String>,
    /// The node and service account names, kept only so that redaction can replace them.
    pub node_name: Option<String>,
    pub service_account: Option<String>,
    pub containers: Vec<ContainerRecord>,
}

/// What kausa keeps of one container, from the Pod's spec and status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainerRecord {
    pub name: String,
    pub init: bool,
    pub image: Option<String>,
    pub waiting_reason: Option<String>,
    pub waiting_message: Option<String>,
    /// The reason of the current state when the container is terminated (no restart).
    pub terminated_reason: Option<String>,
    pub terminated_exit_code: Option<i32>,
    /// The reason and exit code of the previous instance, when the container restarted.
    pub last_termination_reason: Option<String>,
    pub last_termination_exit_code: Option<i32>,
    pub restart_count: i32,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    pub probes: Vec<ProbeRecord>,
}

/// The timing of one probe; never its command, path or headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeRecord {
    pub kind: &'static str,
    pub initial_delay_seconds: Option<i32>,
    pub period_seconds: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub failure_threshold: Option<i32>,
}

impl PodRecord {
    /// Reduces a Pod to its record. `None` when the Pod has no name, namespace or UID.
    pub fn from_pod(pod: &Pod) -> Option<Self> {
        let metadata = &pod.metadata;
        let spec = pod.spec.as_ref();
        let status = pod.status.as_ref();
        let mut containers = Vec::new();
        if let Some(spec) = spec {
            let init_statuses = status.and_then(|s| s.init_container_statuses.as_ref());
            for container in spec.init_containers.iter().flatten() {
                containers.push(ContainerRecord::new(container, true, init_statuses));
            }
            let statuses = status.and_then(|s| s.container_statuses.as_ref());
            for container in &spec.containers {
                containers.push(ContainerRecord::new(container, false, statuses));
            }
        }
        let unschedulable = status
            .and_then(|s| s.conditions.as_ref())
            .and_then(|conditions| {
                conditions.iter().find(|c| {
                    c.type_ == "PodScheduled"
                        && c.status == "False"
                        && c.reason.as_deref() == Some("Unschedulable")
                })
            })
            .map(|c| bounded(c.message.as_deref().unwrap_or_default()));
        Some(Self {
            namespace: metadata.namespace.clone()?,
            name: metadata.name.clone()?,
            uid: metadata.uid.clone()?,
            controller: controller_of(metadata.owner_references.as_ref()),
            phase: status.and_then(|s| s.phase.clone()),
            unschedulable,
            node_name: spec.and_then(|s| s.node_name.clone()),
            service_account: spec.and_then(|s| s.service_account_name.clone()),
            containers,
        })
    }
}

impl ContainerRecord {
    fn new(container: &Container, init: bool, statuses: Option<&Vec<ContainerStatus>>) -> Self {
        let status = statuses.and_then(|all| all.iter().find(|s| s.name == container.name));
        let state = status.and_then(|s| s.state.as_ref());
        let waiting = state.and_then(|s| s.waiting.as_ref());
        let terminated = state.and_then(|s| s.terminated.as_ref());
        let last = status
            .and_then(|s| s.last_state.as_ref())
            .and_then(|s| s.terminated.as_ref());
        let limits = container.resources.as_ref().and_then(|r| r.limits.as_ref());
        let limit = |name: &str| limits.and_then(|l| l.get(name)).map(|q| q.0.clone());
        let probes = [
            ("liveness", container.liveness_probe.as_ref()),
            ("readiness", container.readiness_probe.as_ref()),
            ("startup", container.startup_probe.as_ref()),
        ]
        .into_iter()
        .filter_map(|(kind, probe)| probe.map(|p| ProbeRecord::new(kind, p)))
        .collect();
        Self {
            name: container.name.clone(),
            init,
            image: container.image.clone(),
            waiting_reason: waiting.and_then(|w| w.reason.clone()),
            waiting_message: waiting.and_then(|w| w.message.as_deref().map(bounded)),
            terminated_reason: terminated.and_then(|t| t.reason.clone()),
            terminated_exit_code: terminated.map(|t| t.exit_code),
            last_termination_reason: last.and_then(|t| t.reason.clone()),
            last_termination_exit_code: last.map(|t| t.exit_code),
            restart_count: status.map_or(0, |s| s.restart_count),
            memory_limit: limit("memory"),
            cpu_limit: limit("cpu"),
            probes,
        }
    }
}

impl ProbeRecord {
    fn new(kind: &'static str, probe: &Probe) -> Self {
        Self {
            kind,
            initial_delay_seconds: probe.initial_delay_seconds,
            period_seconds: probe.period_seconds,
            timeout_seconds: probe.timeout_seconds,
            failure_threshold: probe.failure_threshold,
        }
    }
}

/// The object an event is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Regarding {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub uid: Option<String>,
}

impl Regarding {
    fn from_object_reference(reference: &ObjectReference) -> Option<Self> {
        Some(Self {
            api_version: reference.api_version.clone().unwrap_or_default(),
            kind: reference.kind.clone()?,
            namespace: reference.namespace.clone()?,
            name: reference.name.clone()?,
            uid: reference.uid.clone(),
        })
    }
}

/// What kausa keeps of a Warning event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRecord {
    pub uid: String,
    pub reason: String,
    pub note: Option<String>,
    pub regarding: Regarding,
    pub reporting_controller: Option<String>,
    /// When the event first happened: the event time, the deprecated first timestamp that core/v1
    /// writers set, or the creation time, in that order.
    pub first_seen: Option<Timestamp>,
    /// When the event last happened: the series' last observation, the deprecated last timestamp,
    /// the event time or the creation time, in that order.
    pub last_seen: Option<Timestamp>,
    pub count: i32,
}

impl EventRecord {
    /// Reduces an event to its record. `None` when it lacks a UID, a reason or a namespaced
    /// `regarding` object, or when the event's own namespace is missing or differs from the
    /// namespace of its `regarding` object.
    ///
    /// The `events.k8s.io/v1` API accepts an Event whose `regarding` object is in another
    /// namespace. kausa keys incidents, owner lookups, log reads and its own Event on
    /// `regarding.namespace`, so such an Event would let whoever may create Events in one namespace
    /// make kausa act on another. Events about cluster-scoped objects (a Node) have no
    /// `regarding.namespace` and are skipped too; an empty one matches no event namespace.
    pub fn from_event(event: &Event) -> Option<Self> {
        let namespace = event
            .metadata
            .namespace
            .as_deref()
            .filter(|namespace| !namespace.is_empty())?;
        let regarding = Regarding::from_object_reference(event.regarding.as_ref()?)?;
        if regarding.namespace != namespace {
            return None;
        }
        let last_seen = event
            .series
            .as_ref()
            .map(|s| s.last_observed_time.0)
            .or_else(|| event.deprecated_last_timestamp.as_ref().map(|t| t.0))
            .or_else(|| event.event_time.as_ref().map(|t| t.0))
            .or_else(|| event.metadata.creation_timestamp.as_ref().map(|t| t.0));
        let first_seen = event
            .event_time
            .as_ref()
            .map(|t| t.0)
            .or_else(|| event.deprecated_first_timestamp.as_ref().map(|t| t.0))
            .or_else(|| event.metadata.creation_timestamp.as_ref().map(|t| t.0));
        let count = event
            .series
            .as_ref()
            .map(|s| s.count)
            .or(event.deprecated_count)
            .unwrap_or(1);
        Some(Self {
            uid: event.metadata.uid.clone()?,
            reason: event.reason.clone()?,
            note: event.note.as_deref().map(bounded),
            regarding,
            reporting_controller: event.reporting_controller.clone(),
            first_seen,
            last_seen,
            count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Invented values only: no real cluster output (AGENTS.md, Security rules).
    fn crashing_pod() -> Pod {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "api-7d9c8b6f4d-2xkqp",
                "namespace": "shop",
                "uid": "11111111-1111-4111-8111-111111111111",
                "ownerReferences": [
                    {"apiVersion": "v1", "kind": "ConfigMap", "name": "not-a-controller",
                     "uid": "22222222-2222-4222-8222-222222222222"},
                    {"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "api-7d9c8b6f4d",
                     "uid": "33333333-3333-4333-8333-333333333333", "controller": true}
                ]
            },
            "spec": {
                "containers": [{
                    "name": "api",
                    "image": "registry.example.com/shop/api:1.4.2",
                    "env": [{"name": "DB_PASSWORD", "value": "not-a-real-password"}],
                    "command": ["/app", "--token=not-a-real-token"],
                    "resources": {"limits": {"memory": "256Mi", "cpu": "500m"}},
                    "readinessProbe": {"httpGet": {"path": "/ready", "port": 8080},
                                       "periodSeconds": 10, "failureThreshold": 3}
                }]
            },
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "api", "image": "registry.example.com/shop/api:1.4.2", "imageID": "",
                    "ready": false, "restartCount": 6,
                    "state": {"waiting": {"reason": "CrashLoopBackOff",
                                          "message": "back-off 2m40s restarting failed container"}},
                    "lastState": {"terminated": {"reason": "Error", "exitCode": 1}}
                }]
            }
        }))
        .expect("the fixture is a valid Pod")
    }

    #[test]
    fn a_pod_becomes_a_small_record() {
        let record = PodRecord::from_pod(&crashing_pod()).expect("a complete Pod");
        assert_eq!(record.namespace, "shop");
        assert_eq!(record.name, "api-7d9c8b6f4d-2xkqp");
        assert_eq!(
            record
                .controller
                .as_ref()
                .map(|c| (c.kind.as_str(), c.name.as_str())),
            Some(("ReplicaSet", "api-7d9c8b6f4d"))
        );
        assert_eq!(record.phase.as_deref(), Some("Running"));
        let api = &record.containers[0];
        assert!(!api.init);
        assert_eq!(api.waiting_reason.as_deref(), Some("CrashLoopBackOff"));
        assert_eq!(api.last_termination_reason.as_deref(), Some("Error"));
        assert_eq!(api.last_termination_exit_code, Some(1));
        assert_eq!(api.restart_count, 6);
        assert_eq!(api.memory_limit.as_deref(), Some("256Mi"));
        assert_eq!(api.probes.len(), 1);
        assert_eq!(api.probes[0].kind, "readiness");
        assert_eq!(api.probes[0].period_seconds, Some(10));
    }

    #[test]
    fn the_record_drops_environment_and_command_lines() {
        let record = PodRecord::from_pod(&crashing_pod()).expect("a complete Pod");
        // The failure messages name what leaked, not the record: printing a record that holds a
        // secret would log it (CodeQL's rust/cleartext-logging).
        let debug = format!("{record:?}");
        assert!(
            !debug.contains("not-a-real-password"),
            "the password reached the record"
        );
        assert!(
            !debug.contains("not-a-real-token"),
            "the token reached the record"
        );
        assert!(
            !debug.contains("/ready"),
            "the probe path reached the record"
        );
    }

    #[test]
    fn long_messages_are_cut_at_a_character_boundary() {
        let text = "é".repeat(MAX_TEXT_BYTES); // two bytes each
        let cut = bounded(&text);
        assert!(cut.len() <= MAX_TEXT_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(bounded("short"), "short");
    }

    #[test]
    fn an_unschedulable_pod_keeps_the_scheduler_message() {
        let mut pod = crashing_pod();
        let status = pod.status.as_mut().expect("a status");
        status.phase = Some("Pending".into());
        status.conditions = Some(vec![
            serde_json::from_value(json!({
                "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                "message": "0/3 nodes are available: 3 Insufficient memory."
            }))
            .expect("a condition"),
        ]);
        let record = PodRecord::from_pod(&pod).expect("a complete Pod");
        assert_eq!(
            record.unschedulable.as_deref(),
            Some("0/3 nodes are available: 3 Insufficient memory.")
        );
    }

    #[test]
    fn a_pod_without_a_uid_is_skipped() {
        let mut pod = crashing_pod();
        pod.metadata.uid = None;
        assert_eq!(PodRecord::from_pod(&pod), None);
    }

    fn backoff_event() -> Event {
        serde_json::from_value(json!({
            "apiVersion": "events.k8s.io/v1",
            "kind": "Event",
            "metadata": {"name": "api.1", "namespace": "shop",
                         "uid": "44444444-4444-4444-8444-444444444444",
                         "creationTimestamp": "2026-10-26T10:00:00Z"},
            "eventTime": null,
            "reason": "BackOff",
            "note": "Back-off restarting failed container api",
            "type": "Warning",
            "regarding": {"apiVersion": "v1", "kind": "Pod", "namespace": "shop",
                          "name": "api-7d9c8b6f4d-2xkqp",
                          "uid": "11111111-1111-4111-8111-111111111111"},
            "deprecatedFirstTimestamp": "2026-10-26T10:00:00Z",
            "deprecatedLastTimestamp": "2026-10-26T10:05:00Z",
            "deprecatedCount": 7
        }))
        .expect("the fixture is a valid Event")
    }

    #[test]
    fn an_event_in_the_namespace_of_its_object_is_kept() {
        let record = EventRecord::from_event(&backoff_event()).expect("a complete Event");
        assert_eq!(record.regarding.namespace, "shop");
    }

    #[test]
    fn an_event_about_an_object_in_another_namespace_is_skipped() {
        let mut event = backoff_event();
        event.metadata.namespace = Some("tenant".into());
        assert_eq!(EventRecord::from_event(&event), None);
    }

    #[test]
    fn an_event_without_a_namespace_is_skipped() {
        let mut event = backoff_event();
        event.metadata.namespace = None;
        assert_eq!(EventRecord::from_event(&event), None);
        event.metadata.namespace = Some(String::new());
        assert_eq!(EventRecord::from_event(&event), None);
    }

    #[test]
    fn an_event_about_a_cluster_scoped_object_is_skipped() {
        let mut event = backoff_event();
        event.metadata.namespace = Some("default".into());
        let regarding = event.regarding.as_mut().expect("a regarding object");
        regarding.kind = Some("Node".into());
        regarding.namespace = None;
        assert_eq!(EventRecord::from_event(&event), None);
        event
            .regarding
            .as_mut()
            .expect("a regarding object")
            .namespace = Some(String::new());
        assert_eq!(EventRecord::from_event(&event), None);
    }

    #[test]
    fn a_core_event_takes_its_deprecated_timestamp_and_count() {
        let event = backoff_event();
        let record = EventRecord::from_event(&event).expect("a complete Event");
        assert_eq!(record.reason, "BackOff");
        assert_eq!(record.regarding.kind, "Pod");
        assert_eq!(record.count, 7);
        assert_eq!(
            record.first_seen,
            Some("2026-10-26T10:00:00Z".parse().expect("a timestamp"))
        );
        assert_eq!(
            record.last_seen,
            Some("2026-10-26T10:05:00Z".parse().expect("a timestamp"))
        );
    }
}
