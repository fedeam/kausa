//! The context of an incident as it is sent to the model: only the fields of
//! docs/reference/data-sent-to-the-provider.md (`R001`), every name and free text redacted.

use serde_json::{Value, json};

use crate::detect::Detection;
use crate::incident::Incident;
use crate::record::{
    DeploymentRecord, EventRecord, Key, PodAffinityEntry, PodRecord, SchedulingRecord,
};
use crate::redact::{NameKind, Redactor, object_kind};

/// How many Pods, events and log bytes one request carries at most.
const MAX_PODS: usize = 3;
const MAX_EVENTS: usize = 10;
pub const MAX_LOG_BYTES: usize = 4096;

/// What kausa gathered about an incident, before redaction.
#[derive(Clone)]
pub struct Gathered {
    pub incident: Incident,
    /// The incident's Pods that kausa still knows, sample first.
    pub pods: Vec<PodRecord>,
    /// The Warning events of those Pods (or of the object the incident is about, when it is not a
    /// Pod: a Job, a workload controller, a claim, an autoscaler), most recent first.
    pub events: Vec<EventRecord>,
    pub pods_total: usize,
    /// The last lines of the failed container, as read (redacted here).
    pub log_tail: Option<String>,
    /// For `D014`, the record kausa keeps of the stalled Deployment, if it still does.
    pub rollout: Option<DeploymentRecord>,
}

/// Which rule-gated fields a request carries (docs/adr/0017-richer-context-for-the-model.md).
/// A gated key is absent from the request, never `null`.
#[derive(Clone, Copy)]
struct Gates {
    /// A probe's path, for `D001` and `D006`: the rules a probe explains.
    path: bool,
    /// The image pull policy and the number of pull secrets, for `D002`.
    pull: bool,
    /// The scheduling keys of an unschedulable Pod, for `D005`.
    scheduling: bool,
}

impl Gates {
    fn of(detection: Detection) -> Self {
        Self {
            path: matches!(detection, Detection::CrashLoop | Detection::ProbeFailure),
            pull: detection == Detection::ImagePull,
            scheduling: detection == Detection::Unschedulable,
        }
    }
}

/// Builds the redacted context and returns it with the redactor that holds its pseudonyms.
pub fn build(gathered: &Gathered) -> (Value, Redactor) {
    let mut r = Redactor::new();
    let incident = &gathered.incident;
    let gates = Gates::of(incident.key.detection);
    // Register the known names first, so they become placeholders wherever they appear.
    let namespace = r.name(NameKind::Namespace, &incident.key.namespace);
    let kind = object_kind(&incident.owner.kind);
    let workload = r.name(NameKind::Object(kind), &incident.owner.name);
    for pod in &gathered.pods {
        r.name(NameKind::Pod, &pod.name);
        for container in &pod.containers {
            r.name(NameKind::Container, &container.name);
        }
        if let Some(node) = &pod.node_name {
            r.name(NameKind::Node, node);
        }
        if let Some(account) = &pod.service_account {
            r.name(NameKind::ServiceAccount, account);
        }
    }

    let pods: Vec<Value> = gathered
        .pods
        .iter()
        .take(MAX_PODS)
        .map(|pod| pod_context(pod, gates, &mut r))
        .collect();
    // The first Pod that has scheduling keys (the sample comes first); `null` when none has.
    let scheduling = gates.scheduling.then(|| {
        gathered
            .pods
            .iter()
            .find_map(|pod| pod.scheduling.as_ref())
            .map_or(Value::Null, |s| scheduling_context(s, &mut r))
    });
    let events: Vec<Value> = gathered
        .events
        .iter()
        .take(MAX_EVENTS)
        .map(|event| {
            json!({
                "reason": event.reason,
                "message": r.text(event.note.as_deref().unwrap_or_default()),
                "count": event.count,
            })
        })
        .collect();
    let log_tail = gathered.log_tail.as_deref().map(|log| {
        let redacted = r.text(log);
        let start = redacted.len().saturating_sub(MAX_LOG_BYTES);
        let start = (start..=redacted.len())
            .find(|&i| redacted.is_char_boundary(i))
            .unwrap_or(redacted.len());
        redacted[start..].to_owned()
    });

    let mut context = json!({
        "detection": {
            "id": incident.key.detection.id(),
            "name": incident.key.detection.name(),
        },
        "workload": {"kind": kind, "name": workload, "namespace": namespace},
        "pods_affected": incident.pods.len(),
        "pods_total": gathered.pods_total,
        "pods": pods,
        "warning_events": events,
    });
    if let Some(scheduling) = scheduling {
        context["scheduling"] = scheduling;
    }
    if let Some(log_tail) = log_tail {
        context["log_tail"] = Value::from(log_tail);
    }
    if incident.key.detection == Detection::RolloutStalled {
        // Counts only, so nothing here is text to redact; `null` once the record is gone.
        context["rollout"] = gathered
            .rollout
            .as_ref()
            .map_or(Value::Null, rollout_context);
    }
    (context, r)
}

/// The scheduling keys of an unschedulable Pod, for `D005`. A valid key is redacted like a
/// message, an invalid one is sent as the marker `invalid`; operators, effects and the closed
/// `os` and `arch` values are sent as recorded.
fn scheduling_context(s: &SchedulingRecord, r: &mut Redactor) -> Value {
    let mut key = |key: &Key| -> Value {
        match key {
            Key::Valid(text) => Value::from(r.text(text)),
            Key::Invalid => Value::from("invalid"),
        }
    };
    let node_selector: Vec<Value> = s
        .node_selector
        .iter()
        .map(|e| json!({"key": key(&e.key), "value": e.value}))
        .collect();
    let node_affinity: Vec<Value> = s
        .node_affinity
        .iter()
        .map(|e| {
            json!({
                "term": e.term,
                "key": key(&e.key),
                "operator": e.operator,
                "values": e.values.iter().collect::<Vec<_>>(),
                "value_count": e.value_count,
            })
        })
        .collect();
    let mut pod_affinity = |entries: &[PodAffinityEntry]| -> Vec<Value> {
        entries
            .iter()
            .map(|e| {
                json!({"topology_key": key(&e.topology_key), "namespaces_set": e.namespaces_set})
            })
            .collect()
    };
    let affinity = pod_affinity(&s.pod_affinity);
    let anti_affinity = pod_affinity(&s.pod_anti_affinity);
    let tolerations: Vec<Value> = s
        .tolerations
        .iter()
        .map(|e| {
            json!({
                "key": e.key.as_ref().map_or(Value::Null, &mut key),
                "operator": e.operator,
                "effect": e.effect,
            })
        })
        .collect();
    json!({
        "node_selector": node_selector,
        "node_affinity": node_affinity,
        "pod_affinity": affinity,
        "pod_anti_affinity": anti_affinity,
        "tolerations": tolerations,
        "omitted": s.omitted,
    })
}

fn rollout_context(d: &DeploymentRecord) -> Value {
    json!({
        "replicas": d.replicas,
        "updated_replicas": d.updated_replicas,
        "ready_replicas": d.ready_replicas,
        "available_replicas": d.available_replicas,
        "unavailable_replicas": d.unavailable_replicas,
        "progress_deadline_seconds": d.progress_deadline_seconds,
        "min_ready_seconds": d.min_ready_seconds,
    })
}

fn pod_context(pod: &PodRecord, gates: Gates, r: &mut Redactor) -> Value {
    let containers: Vec<Value> = pod
        .containers
        .iter()
        .map(|c| {
            let probes: Vec<Value> = c
                .probes
                .iter()
                .map(|p| {
                    let mut probe = json!({
                        "kind": p.kind,
                        "handler": p.handler,
                        "port": p.port,
                        "initial_delay_seconds": p.initial_delay_seconds,
                        "period_seconds": p.period_seconds,
                        "timeout_seconds": p.timeout_seconds,
                        "success_threshold": p.success_threshold,
                        "failure_threshold": p.failure_threshold,
                    });
                    if gates.path {
                        // Workload-written text: redacted like a message.
                        probe["path"] = Value::from(p.path.as_deref().map(|path| r.text(path)));
                    }
                    probe
                })
                .collect();
            let mut container = json!({
                "name": r.name(NameKind::Container, &c.name),
                "init": c.init,
                "image": c.image.as_deref().map(|i| r.image(i)),
                "waiting_reason": c.waiting_reason,
                "waiting_message": c.waiting_message.as_deref().map(|m| r.text(m)),
                "terminated_reason": c.terminated_reason,
                "terminated_exit_code": c.terminated_exit_code,
                "last_termination_reason": c.last_termination_reason,
                "last_termination_exit_code": c.last_termination_exit_code,
                "restart_count": c.restart_count,
                "memory_request": c.memory_request,
                "cpu_request": c.cpu_request,
                "memory_limit": c.memory_limit,
                "cpu_limit": c.cpu_limit,
                "probes": probes,
            });
            if gates.pull {
                container["image_pull_policy"] = Value::from(c.image_pull_policy);
            }
            container
        })
        .collect();
    let mut sent = json!({
        "name": r.name(NameKind::Pod, &pod.name),
        "phase": pod.phase,
        "unschedulable": pod.unschedulable.as_deref().map(|m| r.text(m)),
        "reason": pod.reason.as_deref().map(|m| r.text(m)),
        "message": pod.message.as_deref().map(|m| r.text(m)),
        "containers": containers,
    });
    if gates.pull {
        sent["image_pull_secrets"] = Value::from(pod.image_pull_secrets);
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detection;
    use crate::incident::{IncidentKey, Signal};
    use crate::record::{ContainerRecord, OwnerRef, Regarding};
    use k8s_openapi::jiff::Timestamp;
    use std::collections::BTreeSet;

    fn gathered() -> Gathered {
        let owner = OwnerRef {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "api".into(),
            uid: "uid-api".into(),
        };
        let key = IncidentKey {
            namespace: "shop".into(),
            owner_kind: "Deployment".into(),
            owner_name: "api".into(),
            detection: Detection::CrashLoop,
        };
        let sample = Signal {
            key: key.clone(),
            owner: owner.clone(),
            observed_at: Timestamp::UNIX_EPOCH,
            event: None,
            pod: Some("api-7d9c8b6f4d-2xkqp".into()),
            container: Some("api".into()),
            image: None,
            reason: "CrashLoopBackOff".into(),
            message: String::new(),
            exit_code: Some(1),
        };
        Gathered {
            incident: Incident {
                key,
                owner,
                fingerprint: "0".into(),
                first_seen: Timestamp::UNIX_EPOCH,
                last_seen: Timestamp::UNIX_EPOCH,
                pods: BTreeSet::from(["api-7d9c8b6f4d-2xkqp".to_owned()]),
                sample,
            },
            pods: vec![PodRecord {
                namespace: "shop".into(),
                name: "api-7d9c8b6f4d-2xkqp".into(),
                uid: "uid-pod".into(),
                controller: None,
                phase: Some("Running".into()),
                unschedulable: None,
                unschedulable_reason: None,
                reason: None,
                message: None,
                failed_at: None,
                node_name: Some("worker-7".into()),
                service_account: Some("api-runner".into()),
                image_pull_secrets: 0,
                scheduling: None,
                containers: vec![ContainerRecord {
                    name: "api".into(),
                    image: Some("registry.example.com/shop/api:1.4.2".into()),
                    waiting_reason: Some("CrashLoopBackOff".into()),
                    waiting_message: Some(
                        "back-off 40s restarting failed container=api pod=api-7d9c8b6f4d-2xkqp_shop"
                            .into(),
                    ),
                    last_termination_exit_code: Some(1),
                    restart_count: 4,
                    memory_limit: Some("256Mi".into()),
                    ..Default::default()
                }],
            }],
            events: vec![EventRecord {
                uid: "e1".into(),
                reason: "BackOff".into(),
                note: Some(
                    "Back-off restarting failed container api in pod api-7d9c8b6f4d-2xkqp on worker-7"
                        .into(),
                ),
                regarding: Regarding {
                    api_version: "v1".into(),
                    kind: "Pod".into(),
                    namespace: "shop".into(),
                    name: "api-7d9c8b6f4d-2xkqp".into(),
                    uid: None,
                },
                reporting_controller: None,
                first_seen: None,
                last_seen: None,
                count: 4,
            }],
            pods_total: 2,
            log_tail: Some(
                "connecting to postgres://app:hunter2@db.shop.svc.cluster.local:5432\n\
                 Error: connect ECONNREFUSED 192.0.2.10:5432"
                    .into(),
            ),
            rollout: None,
        }
    }

    #[test]
    fn no_real_name_address_or_credential_leaves() {
        let (context, r) = build(&gathered());
        let sent = context.to_string();
        for leaked in [
            "shop",
            "api-7d9c8b6f4d",
            "registry.example.com",
            "hunter2",
            "192.0.2.10",
            "db.shop.svc",
            "worker-7",
            "api-runner",
        ] {
            assert!(!sent.contains(leaked), "{leaked} in {sent}");
        }
        assert_eq!(
            context["workload"],
            json!({"kind": "Deployment", "name": "deployment-1", "namespace": "namespace-1"})
        );
        assert_eq!(
            context["pods"][0]["containers"][0]["image"],
            "registry-1/image-1:1.4.2"
        );
        assert_eq!(context["pods"][0]["containers"][0]["memory_limit"], "256Mi");
        assert!(sent.contains("<redacted:ip>:5432"), "{sent}");
        assert!(r.counts().contains_key("R006"));

        // A Pod that cannot be scheduled sends its keys, never the values of its node selector,
        // affinity or tolerations (docs/adr/0017-richer-context-for-the-model.md).
        let pod = recorded(
            &spec(
                json!({}),
                json!({
                    "nodeSelector": {"example.com/pool": "not-a-real-pool-value",
                                     "kubernetes.io/hostname": "not-a-real-node-value"},
                    "affinity": {"nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                        "nodeSelectorTerms": [{"matchExpressions": [
                            {"key": "topology.kubernetes.io/zone", "operator": "In",
                             "values": ["not-a-real-zone-value"]}
                        ]}]
                    }}},
                    "tolerations": [{"key": "example.com/dedicated", "operator": "Equal",
                                     "value": "not-a-real-toleration-value",
                                     "effect": "NoSchedule"}]
                }),
            ),
            true,
        );
        let (context, _) = build(&with_pods(Detection::Unschedulable, vec![pod]));
        assert!(context["scheduling"].is_object(), "no scheduling sent");
        let sent = context.to_string();
        for (leaked, what) in [
            ("not-a-real-pool-value", "a node selector value"),
            ("not-a-real-node-value", "a kubernetes.io/hostname value"),
            ("not-a-real-zone-value", "an affinity value"),
            ("not-a-real-toleration-value", "a toleration value"),
            ("shop", "the namespace"),
            ("api-7d9c8b6f4d", "the Pod's name"),
            ("server", "the container's name"),
        ] {
            assert!(!sent.contains(leaked), "{what} was sent");
        }
        assert!(
            sent.contains("example.com/pool"),
            "the selector key was not sent"
        );
        assert!(
            sent.contains("example.com/dedicated"),
            "the toleration key was not sent"
        );
    }

    #[test]
    fn the_same_incident_gives_the_same_request() {
        assert_eq!(build(&gathered()).0, build(&gathered()).0);
        // With a scheduling object, its keys redacted.
        let incident = || with_pods(Detection::Unschedulable, vec![recorded(&rich_spec(), true)]);
        let (first, _) = build(&incident());
        assert!(first["scheduling"].is_object(), "no scheduling sent");
        assert_eq!(first, build(&incident()).0);

        // Every list of the scheduling object, with placeholders in its keys: the same numbering
        // each time.
        let full = || {
            let pod = recorded(
                &spec(
                    json!({}),
                    json!({
                        "nodeSelector": {"eks.amazonaws.com/nodegroup": "v",
                                         "example.com/server": "v", "kubernetes.io/os": "linux"},
                        "affinity": {
                            "nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                                "nodeSelectorTerms": [
                                    {"matchExpressions": [{"key": "kubernetes.io/arch",
                                                           "operator": "In",
                                                           "values": ["arm64", "amd64"]}]},
                                    {"matchFields": [{"key": "metadata.name",
                                                      "operator": "NotIn",
                                                      "values": ["v"]}]}
                                ]
                            }},
                            "podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [
                                {"topologyKey": "shop.example.com/zone"}
                            ]},
                            "podAntiAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [
                                {"topologyKey": "kubernetes.io/hostname", "namespaces": ["v"]}
                            ]}
                        },
                        "tolerations": [
                            {"key": "example.com/api", "operator": "Equal", "value": "v",
                             "effect": "NoSchedule"},
                            {"operator": "Exists"}
                        ]
                    }),
                ),
                true,
            );
            with_pods(Detection::Unschedulable, vec![pod])
        };
        let (first, _) = build(&full());
        let scheduling = &first["scheduling"];
        for list in [
            "node_selector",
            "node_affinity",
            "pod_affinity",
            "pod_anti_affinity",
            "tolerations",
        ] {
            assert!(
                scheduling[list].as_array().is_some_and(|l| !l.is_empty()),
                "{list} is empty"
            );
        }
        assert_eq!(
            scheduling["node_selector"][0]["key"], "host-1/nodegroup",
            "the cloud prefix was not replaced"
        );
        assert_eq!(first, build(&full()).0);
    }

    #[test]
    fn custom_owner_kinds_do_not_leak() {
        let mut g = gathered();
        g.incident.owner.kind = "Rollout".into();
        let (context, _) = build(&g);
        assert_eq!(context["workload"]["kind"], "Object");
        assert_eq!(context["workload"]["name"], "object-1");
    }

    #[test]
    fn the_log_tail_keeps_its_end_within_the_cap() {
        let mut g = gathered();
        g.log_tail = Some(format!("{}\nlast line", "x".repeat(MAX_LOG_BYTES * 2)));
        let (context, _) = build(&g);
        let tail = context["log_tail"].as_str().expect("a log tail");
        assert!(tail.len() <= MAX_LOG_BYTES);
        assert!(tail.ends_with("last line"));
    }

    #[test]
    fn an_eviction_message_reaches_the_model_only_redacted() {
        // The kubelet's status message is what tells a Pod's own ephemeral-storage limit from
        // node pressure; it is sent like the other messages, through redaction.
        let evicted = |message: &str| -> PodRecord {
            let pod = serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {"name": "api-7d9c8b6f4d-2xkqp", "namespace": "shop",
                             "uid": "11111111-1111-4111-8111-111111111111"},
                "spec": {"nodeName": "worker-7",
                         "containers": [{"name": "filler", "image": "busybox:1.37"}]},
                "status": {
                    "phase": "Failed",
                    "reason": "Evicted",
                    "message": message,
                    "containerStatuses": [{
                        "name": "filler", "image": "busybox:1.37", "imageID": "",
                        "ready": false, "restartCount": 0,
                        "state": {"terminated": {"reason": "Error", "exitCode": 137}}
                    }]
                }
            }))
            .expect("the fixture is a valid Pod");
            PodRecord::from_pod(&pod).expect("a complete Pod")
        };
        for (message, kept) in [
            (
                "Pod ephemeral local storage usage exceeds the total limit of containers 50Mi. ",
                "exceeds the total limit of containers 50Mi",
            ),
            (
                "The node was low on resource: ephemeral-storage. Threshold quantity: 14711Mi, \
                 available: 14523Mi. Container filler was using 48Ki, request is 0, has larger \
                 consumption of ephemeral-storage. ",
                "The node was low on resource: ephemeral-storage",
            ),
        ] {
            let mut g = gathered();
            g.pods = vec![evicted(message)];
            g.log_tail = None;
            let (context, _) = build(&g);
            let sent = context.to_string();
            assert!(sent.contains(kept), "{sent}");
            assert!(sent.contains("\"Evicted\""), "{sent}");
            for leaked in ["shop", "api-7d9c8b6f4d", "filler", "worker-7"] {
                assert!(!sent.contains(leaked), "{leaked} in {sent}");
            }
        }
    }

    #[test]
    fn the_pod_status_reason_is_redacted_like_the_message() {
        // Kubernetes sets one-word reasons such as `Evicted`, but any writer of pods/status can
        // set any text there.
        let mut g = gathered();
        g.log_tail = None;
        g.pods[0].reason = Some("Evicted".into());
        let (context, _) = build(&g);
        assert_eq!(context["pods"][0]["reason"], "Evicted");

        g.pods[0].reason =
            Some("Drained api-7d9c8b6f4d-2xkqp from 192.0.2.7 password=hunter2-example".into());
        let (context, _) = build(&g);
        let reason = context["pods"][0]["reason"].as_str().expect("a reason");
        assert!(reason.starts_with("Drained "), "{reason}");
        for leaked in ["api-7d9c8b6f4d", "192.0.2.7", "hunter2"] {
            assert!(!reason.contains(leaked), "{leaked} in {reason}");
        }
    }

    // Acceptance tests for `D011`, `D012` and `D013`, written before the code. Their incidents
    // have no Pod: the request carries the object the incident is about and the notes of its
    // events, which are cluster text and reach the model only through redaction. The notes follow
    // what the controllers wrote on kind (Kubernetes 1.37, 2026-10-28); names are invented.

    /// The request for an incident about `owner` whose events, about `regarding`, are `events`
    /// (reason and note), the first one the sample. Returns the context and the events' messages
    /// as sent.
    fn about(
        (api_version, kind, name): (&str, &str, &str),
        regarding: (&str, &str, &str),
        events: &[(&str, &str)],
    ) -> (Value, Vec<String>) {
        let owner = OwnerRef {
            api_version: api_version.into(),
            kind: kind.into(),
            name: name.into(),
            uid: format!("uid-{name}"),
        };
        let records: Vec<EventRecord> = events
            .iter()
            .map(|(reason, note)| EventRecord {
                uid: format!("e-{reason}"),
                reason: (*reason).into(),
                note: Some((*note).into()),
                regarding: Regarding {
                    api_version: regarding.0.into(),
                    kind: regarding.1.into(),
                    namespace: "shop".into(),
                    name: regarding.2.into(),
                    uid: Some(format!("uid-{}", regarding.2)),
                },
                reporting_controller: None,
                first_seen: None,
                last_seen: None,
                count: 12,
            })
            .collect();
        let first = &records[0];
        let detection = crate::detect::detect_event(first).expect("a detection for the event");
        let key = IncidentKey {
            namespace: "shop".into(),
            owner_kind: kind.into(),
            owner_name: name.into(),
            detection,
        };
        let sample = Signal {
            key: key.clone(),
            owner: owner.clone(),
            observed_at: Timestamp::UNIX_EPOCH,
            event: Some((first.uid.clone(), None)),
            pod: None,
            container: None,
            image: None,
            reason: first.reason.clone(),
            message: first.note.clone().unwrap_or_default(),
            exit_code: None,
        };
        let gathered = Gathered {
            incident: Incident {
                key,
                owner,
                fingerprint: "0".into(),
                first_seen: Timestamp::UNIX_EPOCH,
                last_seen: Timestamp::UNIX_EPOCH,
                pods: BTreeSet::new(),
                sample,
            },
            pods: Vec::new(),
            events: records,
            pods_total: 0,
            log_tail: None,
            rollout: None,
        };
        let (context, _) = build(&gathered);
        let messages = context["warning_events"]
            .as_array()
            .expect("the warning events")
            .iter()
            .map(|e| e["message"].as_str().expect("a message").to_owned())
            .collect();
        (context, messages)
    }

    /// Asserts that no name in `leaked` is in the context, and that each message keeps the words
    /// that say what failed.
    fn redacted(context: &Value, messages: &[String], leaked: &[&str], kept: &[&str]) {
        let sent = context.to_string();
        for name in leaked {
            assert!(!sent.contains(name), "{name} in {sent}");
        }
        assert_eq!(messages.len(), kept.len(), "{messages:?}");
        for (message, words) in messages.iter().zip(kept) {
            assert!(message.contains(words), "{words} not in {message}");
        }
        assert!(context["pods"].as_array().is_some_and(Vec::is_empty));
        assert!(context.get("log_tail").is_none());
    }

    #[test]
    fn a_d011_note_reaches_the_model_redacted() {
        // A quota refusal names the Pod the ReplicaSet tried to create and the quota; a Pod
        // Security refusal names the containers.
        let (context, messages) = about(
            ("apps/v1", "Deployment", "checkout"),
            ("apps/v1", "ReplicaSet", "checkout-7fffb9bf5c"),
            &[
                (
                    "FailedCreate",
                    "Error creating: pods \"checkout-7fffb9bf5c-d98v6\" is forbidden: exceeded \
                     quota: compute-budget, requested: pods=1, used: pods=3, limited: pods=3",
                ),
                (
                    "FailedCreate",
                    "(combined from similar events): Error creating: pods \
                     \"checkout-7fffb9bf5c-7frqt\" is forbidden: violates PodSecurity \
                     \"restricted:latest\": privileged (containers \"collector\", \"shipper\" \
                     must not set securityContext.privileged=true), runAsNonRoot != true (pod or \
                     container \"collector\" must set securityContext.runAsNonRoot=true)",
                ),
            ],
        );
        assert_eq!(context["detection"]["id"], "D011");
        assert_eq!(
            context["workload"],
            json!({"kind": "Deployment", "name": "deployment-1", "namespace": "namespace-1"})
        );
        redacted(
            &context,
            &messages,
            &[
                "shop",
                "checkout",
                "7fffb9bf5c",
                "d98v6",
                "7frqt",
                "compute-budget",
                "collector",
                "shipper",
            ],
            &[
                "is forbidden: exceeded quota",
                "violates PodSecurity \"restricted:latest\": privileged",
            ],
        );
        assert!(messages[0].contains("limited: pods=3"), "{}", messages[0]);
        assert!(
            messages[1].contains("must not set securityContext.privileged=true"),
            "{}",
            messages[1]
        );
    }

    #[test]
    fn a_d012_note_reaches_the_model_redacted() {
        // The PersistentVolume controller and external provisioners name the StorageClass.
        let claim = ("v1", "PersistentVolumeClaim", "orders-data");
        let (context, messages) = about(
            claim,
            claim,
            &[
                (
                    "ProvisioningFailed",
                    r#"storageclass.storage.k8s.io "fast-ssd" not found"#,
                ),
                (
                    "ProvisioningFailed",
                    "failed to provision volume with StorageClass \"fast-ssd\": NodePath only \
                     supports ReadWriteOnce and ReadWriteOncePod (1.22+) access modes",
                ),
            ],
        );
        assert_eq!(context["detection"]["id"], "D012");
        // A built-in kind is sent as it is (`Object` is for custom resources only).
        assert_eq!(
            context["workload"],
            json!({
                "kind": "PersistentVolumeClaim",
                "name": "persistentvolumeclaim-1",
                "namespace": "namespace-1"
            })
        );
        redacted(
            &context,
            &messages,
            &["shop", "orders-data", "fast-ssd"],
            &[
                "not found",
                "NodePath only supports ReadWriteOnce and ReadWriteOncePod",
            ],
        );
    }

    #[test]
    fn a_d013_note_reaches_the_model_redacted() {
        // An autoscaler whose target does not exist names the target; one with no metrics API
        // names only the resource.
        let hpa = ("autoscaling/v2", "HorizontalPodAutoscaler", "checkout");
        let (context, messages) = about(
            hpa,
            hpa,
            &[
                (
                    "FailedGetScale",
                    r#"deployments.apps "storefront" not found"#,
                ),
                (
                    "FailedGetResourceMetric",
                    "failed to get cpu utilization: unable to get metrics for resource cpu: \
                     unable to fetch metrics from resource metrics API: the server could not \
                     find the requested resource (get pods.metrics.k8s.io)",
                ),
            ],
        );
        assert_eq!(context["detection"]["id"], "D013");
        assert_eq!(
            context["workload"],
            json!({
                "kind": "HorizontalPodAutoscaler",
                "name": "horizontalpodautoscaler-1",
                "namespace": "namespace-1"
            })
        );
        redacted(
            &context,
            &messages,
            &["shop", "checkout", "storefront"],
            &["not found", "unable to get metrics for resource cpu"],
        );
    }

    // Acceptance tests for docs/adr/0017-richer-context-for-the-model.md, written before the
    // code: which rule gets which new field, and how paths and keys are redacted. They read
    // `incident.key.detection` for the gates and use the record of `src/record.rs`
    // (`PodRecord::from_pod` with `scheduling` and the new container and probe fields). Values
    // are invented.

    use k8s_openapi::api::core::v1::Pod;

    const ALL_RULES: [Detection; 13] = [
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

    /// A Pod spec with one container `server` with `fields`, and `extra` in the spec.
    fn spec(fields: Value, extra: Value) -> Value {
        let mut container = json!({"name": "server", "image": "busybox:1.37"});
        let object = container.as_object_mut().expect("an object");
        for (name, value) in fields.as_object().expect("the fields are an object") {
            object.insert(name.clone(), value.clone());
        }
        let mut spec = json!({"containers": [container]});
        let object = spec.as_object_mut().expect("an object");
        for (name, value) in extra.as_object().expect("the extra fields are an object") {
            object.insert(name.clone(), value.clone());
        }
        spec
    }

    /// The record of Pod `api-7d9c8b6f4d-2xkqp` in `shop` with `spec`, unschedulable or running.
    fn recorded(spec: &Value, unschedulable: bool) -> PodRecord {
        let status = if unschedulable {
            json!({"phase": "Pending",
                   "conditions": [{"type": "PodScheduled", "status": "False",
                                   "reason": "Unschedulable",
                                   "message": "0/3 nodes are available: 3 node(s) didn't match \
                                               Pod's node affinity/selector."}]})
        } else {
            json!({"phase": "Running"})
        };
        let pod: Pod = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "api-7d9c8b6f4d-2xkqp", "namespace": "shop",
                         "uid": "11111111-1111-4111-8111-111111111111"},
            "spec": spec,
            "status": status
        }))
        .expect("the fixture is a valid Pod");
        PodRecord::from_pod(&pod).expect("a complete Pod")
    }

    /// A Pod with every new field: requests, a pull policy, a named HTTP probe port and a path
    /// with a query, two pull secrets and a node selector.
    fn rich_spec() -> Value {
        spec(
            json!({
                "imagePullPolicy": "IfNotPresent",
                "ports": [{"name": "http", "containerPort": 8080}],
                "resources": {"requests": {"memory": "128Mi", "cpu": "250m"},
                              "limits": {"memory": "256Mi", "cpu": "500m"}},
                "readinessProbe": {
                    "httpGet": {"path": "/ready?probe-query=x", "port": "http", "scheme": "HTTP"},
                    "periodSeconds": 10, "timeoutSeconds": 1, "successThreshold": 1,
                    "failureThreshold": 3
                }
            }),
            json!({
                "imagePullSecrets": [{"name": "pull-one"}, {"name": "pull-two"}],
                "nodeSelector": {"disktype": "ssd"}
            }),
        )
    }

    /// The incident of `gathered()` with `detection` and `pods`, without events or a log tail.
    fn with_pods(detection: Detection, pods: Vec<PodRecord>) -> Gathered {
        let mut g = gathered();
        g.incident.key.detection = detection;
        g.incident.sample.key.detection = detection;
        g.pods = pods;
        g.events = Vec::new();
        g.log_tail = None;
        g
    }

    /// The keys of a JSON object, sorted.
    fn keys(value: &Value) -> Vec<String> {
        let mut keys: Vec<String> = value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn each_rule_gets_its_fields() {
        let pod = recorded(&rich_spec(), true);
        for detection in ALL_RULES {
            let id = detection.id();
            let (context, _) = build(&with_pods(detection, vec![pod.clone()]));
            let sent_pod = &context["pods"][0];
            let container = &sent_pod["containers"][0];
            let probe = &container["probes"][0];

            // Every rule: requests and the probe's shape.
            assert_eq!(container["memory_request"], "128Mi", "{id}");
            assert_eq!(container["cpu_request"], "250m", "{id}");
            assert_eq!(container["memory_limit"], "256Mi", "{id}");
            assert_eq!(container["cpu_limit"], "500m", "{id}");
            let path = matches!(detection, Detection::CrashLoop | Detection::ProbeFailure);
            let mut expected = vec![
                "failure_threshold",
                "handler",
                "initial_delay_seconds",
                "kind",
                "period_seconds",
                "port",
                "success_threshold",
                "timeout_seconds",
            ];
            if path {
                expected.push("path");
                expected.sort_unstable();
            }
            assert_eq!(keys(probe), expected, "{id}");
            assert_eq!(probe["kind"], "readiness", "{id}");
            assert_eq!(probe["handler"], "http", "{id}");
            assert_eq!(probe["port"], 8080, "{id}");
            assert_eq!(probe["success_threshold"], 1, "{id}");
            assert_eq!(probe["initial_delay_seconds"], Value::Null, "{id}");
            if path {
                assert_eq!(probe["path"], "/ready", "{id}");
            }

            // D002 only: the pull policy and the number of pull secrets. Absent elsewhere.
            let pull = detection == Detection::ImagePull;
            let container = container.as_object().expect("a container");
            let sent_pod = sent_pod.as_object().expect("a Pod");
            assert_eq!(container.contains_key("image_pull_policy"), pull, "{id}");
            assert_eq!(sent_pod.contains_key("image_pull_secrets"), pull, "{id}");
            if pull {
                assert_eq!(container["image_pull_policy"], "IfNotPresent");
                assert_eq!(sent_pod["image_pull_secrets"], 2);
            }

            // D005 only: the scheduling object. Absent elsewhere.
            let scheduling = detection == Detection::Unschedulable;
            let top = context.as_object().expect("a context");
            assert_eq!(top.contains_key("scheduling"), scheduling, "{id}");
            if scheduling {
                assert!(context["scheduling"].is_object(), "{id}");
            }
        }

        // The rules that turn on request against limit get both.
        for detection in [Detection::OutOfMemory, Detection::PodEvicted] {
            let (context, _) = build(&with_pods(detection, vec![pod.clone()]));
            let container = &context["pods"][0]["containers"][0];
            assert_eq!(
                (
                    &container["memory_request"],
                    &container["memory_limit"],
                    &container["probes"][0]["handler"]
                ),
                (&json!("128Mi"), &json!("256Mi"), &json!("http")),
                "{}",
                detection.id()
            );
        }

        // D005 when no Pod has a scheduling record, or there is no Pod: null, not absent.
        for pods in [vec![recorded(&rich_spec(), false)], Vec::new()] {
            let (context, _) = build(&with_pods(Detection::Unschedulable, pods));
            let top = context.as_object().expect("a context");
            assert_eq!(top.get("scheduling"), Some(&Value::Null));
        }

        // Unset values are null: no requests, an unknown pull policy, a probe without a path.
        let bare = recorded(
            &spec(
                json!({"imagePullPolicy": "Sometimes",
                       "livenessProbe": {"tcpSocket": {"port": 8080}}}),
                json!({}),
            ),
            false,
        );
        for detection in [Detection::ImagePull, Detection::ProbeFailure] {
            let (context, _) = build(&with_pods(detection, vec![bare.clone()]));
            let container = context["pods"][0]["containers"][0]
                .as_object()
                .expect("a container")
                .clone();
            assert_eq!(container.get("memory_request"), Some(&Value::Null));
            assert_eq!(container.get("cpu_request"), Some(&Value::Null));
            let probe = container["probes"][0].as_object().expect("a probe");
            assert_eq!(probe.get("handler"), Some(&json!("tcp")));
            if detection == Detection::ImagePull {
                assert_eq!(container.get("image_pull_policy"), Some(&Value::Null));
                assert_eq!(context["pods"][0]["image_pull_secrets"], 0);
            } else {
                assert_eq!(probe.get("path"), Some(&Value::Null));
            }
        }
    }

    #[test]
    fn d005_sends_the_scheduling_keys_of_the_first_pod_that_has_them() {
        let first = recorded(
            &spec(
                json!({}),
                json!({
                    "nodeSelector": {"disktype": "ssd", "kubernetes.io/os": "linux"},
                    "affinity": {
                        "nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                            "nodeSelectorTerms": [{"matchExpressions": [
                                {"key": "kubernetes.io/arch", "operator": "In",
                                 "values": ["arm64", "amd64", "sparc"]}
                            ]}]
                        }},
                        "podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
                            "topologyKey": "topology.kubernetes.io/zone",
                            "labelSelector": {"matchLabels": {"app": "cache"}}
                        }]},
                        "podAntiAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
                            "topologyKey": "kubernetes.io/hostname",
                            "namespaces": ["other-team"]
                        }]}
                    },
                    "tolerations": [
                        {"key": "example.com/dedicated", "operator": "Equal", "value": "gpu",
                         "effect": "NoSchedule"},
                        {"operator": "Exists"}
                    ]
                }),
            ),
            true,
        );
        let second = recorded(
            &spec(
                json!({}),
                json!({"nodeSelector": {"example.com/second": "v"}}),
            ),
            true,
        );
        let running = recorded(&rich_spec(), false);
        let (context, _) = build(&with_pods(
            Detection::Unschedulable,
            vec![running, first, second],
        ));
        assert_eq!(
            context["scheduling"],
            json!({
                "node_selector": [
                    {"key": "disktype", "value": null},
                    {"key": "kubernetes.io/os", "value": "linux"}
                ],
                "node_affinity": [
                    {"term": 0, "key": "kubernetes.io/arch", "operator": "In",
                     "values": ["amd64", "arm64"], "value_count": 3}
                ],
                "pod_affinity": [
                    {"topology_key": "topology.kubernetes.io/zone", "namespaces_set": false}
                ],
                "pod_anti_affinity": [
                    {"topology_key": "kubernetes.io/hostname", "namespaces_set": true}
                ],
                "tolerations": [
                    {"key": "example.com/dedicated", "operator": "Equal", "effect": "NoSchedule"},
                    {"key": null, "operator": "Exists", "effect": null}
                ],
                "omitted": 0
            })
        );
        let sent = context.to_string();
        for (leaked, what) in [
            ("ssd", "a selector value"),
            ("sparc", "an affinity value outside the closed list"),
            ("cache", "a label selector"),
            ("other-team", "an affinity namespace"),
            ("gpu", "a toleration value"),
        ] {
            assert!(!sent.contains(leaked), "{what} was sent");
        }
    }

    #[test]
    fn names_in_paths_and_keys_are_replaced() {
        // The namespace `shop`, the Deployment `api`, the Pod `api-7d9c8b6f4d-2xkqp` and the
        // container `server` are known names.
        let probe = json!({"httpGet": {"path": "/shop/server/api-7d9c8b6f4d-2xkqp/ready",
                                       "port": 8080}});
        let pod = recorded(&spec(json!({"readinessProbe": probe}), json!({})), false);
        let (context, _) = build(&with_pods(Detection::ProbeFailure, vec![pod]));
        assert_eq!(
            context["pods"][0]["containers"][0]["probes"][0]["path"],
            "/namespace-1/container-1/pod-1/ready"
        );

        let pod = recorded(
            &spec(
                json!({}),
                json!({
                    "nodeSelector": {"example.com/api-7d9c8b6f4d-2xkqp": "v",
                                     "example.com/server": "v", "shop": "v"},
                    "affinity": {"podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [
                        {"topologyKey": "shop.example.com/zone"}
                    ]}},
                    "tolerations": [{"key": "example.com/api", "operator": "Exists"}]
                }),
            ),
            true,
        );
        let (context, _) = build(&with_pods(Detection::Unschedulable, vec![pod]));
        let scheduling = &context["scheduling"];
        assert_eq!(
            scheduling["node_selector"],
            json!([
                {"key": "example.com/pod-1", "value": null},
                {"key": "example.com/container-1", "value": null},
                {"key": "namespace-1", "value": null}
            ])
        );
        assert_eq!(
            scheduling["pod_affinity"][0]["topology_key"],
            "namespace-1.example.com/zone"
        );
        assert_eq!(
            scheduling["tolerations"][0]["key"],
            "example.com/deployment-1"
        );
    }

    #[test]
    fn long_random_paths_and_keys_are_redacted() {
        // A run of 40 distinct letters and digits, built here: high entropy, never issued.
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let run: String = (0..40)
            .map(|i| char::from(ALPHABET[(i * 7) % ALPHABET.len()]))
            .collect();

        let probe = json!({"httpGet": {"path": format!("/hooks/{run}/ready"), "port": 8080}});
        let pod = recorded(&spec(json!({"readinessProbe": probe}), json!({})), false);
        let (context, r) = build(&with_pods(Detection::ProbeFailure, vec![pod]));
        let path = context["pods"][0]["containers"][0]["probes"][0]["path"]
            .as_str()
            .expect("a path");
        assert!(
            !path.contains(&run),
            "the random segment of the path was sent"
        );
        assert!(
            path.contains("<redacted:secret>"),
            "no secret marker in the path"
        );
        assert!(
            r.counts().contains_key("R012"),
            "R012 did not count the path"
        );

        let pod = recorded(
            &spec(
                json!({}),
                json!({"nodeSelector": {format!("example.com/{run}"): "v"}}),
            ),
            true,
        );
        let (context, r) = build(&with_pods(Detection::Unschedulable, vec![pod]));
        let key = context["scheduling"]["node_selector"][0]["key"]
            .as_str()
            .expect("a key");
        assert!(!key.contains(&run), "the random part of the key was sent");
        assert!(
            key.contains("<redacted:secret>"),
            "no secret marker in the key"
        );
        assert!(
            r.counts().contains_key("R012"),
            "R012 did not count the key"
        );
    }

    #[test]
    fn key_prefixes_follow_the_hostname_rule() {
        let pod = recorded(
            &spec(
                json!({}),
                json!({
                    "nodeSelector": {
                        "bad key": "v",
                        "disktype": "v",
                        "eks.amazonaws.com/nodegroup": "v",
                        "example.com/pool": "v",
                        "karpenter.sh/capacity-type": "v",
                        "topology.kubernetes.io/zone": "v"
                    },
                    "tolerations": [{"key": "bad key", "operator": "Exists"}]
                }),
            ),
            true,
        );
        let (context, r) = build(&with_pods(Detection::Unschedulable, vec![pod]));
        let selector: Vec<&str> = context["scheduling"]["node_selector"]
            .as_array()
            .expect("the node selector")
            .iter()
            .map(|e| e["key"].as_str().expect("a key"))
            .collect();
        // A cloud provider's prefix becomes a host placeholder (`R013`); a public host's, a
        // suffix `R013` does not list (a known gap) and a key without a prefix stay; an invalid
        // key is sent as the marker `invalid`.
        assert_eq!(
            selector,
            [
                "invalid",
                "disktype",
                "host-1/nodegroup",
                "example.com/pool",
                "karpenter.sh/capacity-type",
                "topology.kubernetes.io/zone",
            ]
        );
        assert_eq!(context["scheduling"]["tolerations"][0]["key"], "invalid");
        assert!(
            r.counts().contains_key("R013"),
            "R013 did not count the host"
        );
    }

    // Acceptance tests for `D014` (docs/adr/0018-stuck-rollouts.md), written before the code: the
    // request for a stalled rollout carries the Deployment's replica counts as a top-level
    // `rollout` of integers or null, no condition message and no Pod; any other request carries
    // no `rollout` at all. Names and numbers are invented.

    fn stalled_rollout() -> crate::record::DeploymentRecord {
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
            progressing_status: Some("False".into()),
            progressing_reason: Some("ProgressDeadlineExceeded".into()),
        }
    }

    /// What `run.rs` gathers for the `D014` incident of Deployment `checkout` in `shop`: no Pod,
    /// no event, no log, and the record it keeps of the Deployment, if it still does.
    fn rollout_gathered(rollout: Option<crate::record::DeploymentRecord>) -> Gathered {
        let owner = OwnerRef {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "checkout".into(),
            uid: "uid-checkout".into(),
        };
        let key = IncidentKey {
            namespace: "shop".into(),
            owner_kind: "Deployment".into(),
            owner_name: "checkout".into(),
            detection: Detection::RolloutStalled,
        };
        let sample = Signal {
            key: key.clone(),
            owner: owner.clone(),
            observed_at: Timestamp::UNIX_EPOCH,
            event: None,
            pod: None,
            container: None,
            image: None,
            reason: "ProgressDeadlineExceeded".into(),
            message: "revision 7".into(),
            exit_code: None,
        };
        Gathered {
            incident: Incident {
                key,
                owner,
                fingerprint: "0".into(),
                first_seen: Timestamp::UNIX_EPOCH,
                last_seen: Timestamp::UNIX_EPOCH,
                pods: BTreeSet::new(),
                sample,
            },
            pods: Vec::new(),
            events: Vec::new(),
            pods_total: 2,
            log_tail: None,
            rollout,
        }
    }

    #[test]
    fn a_stalled_rollout_sends_its_replica_counts_as_integers() {
        let (context, _) = build(&rollout_gathered(Some(stalled_rollout())));
        assert_eq!(
            context["detection"],
            json!({"id": "D014", "name": "rollout does not progress"})
        );
        assert_eq!(
            context["workload"],
            json!({"kind": "Deployment", "name": "deployment-1", "namespace": "namespace-1"})
        );
        assert_eq!(context["pods_affected"], 0);
        assert_eq!(context["pods_total"], 2);
        assert_eq!(context["pods"], json!([]));
        assert_eq!(context["warning_events"], json!([]));
        assert!(context.get("log_tail").is_none(), "a log tail was sent");
        assert_eq!(
            context["rollout"],
            json!({
                "replicas": 3,
                "updated_replicas": 1,
                "ready_replicas": 2,
                "available_replicas": 2,
                "unavailable_replicas": 1,
                "progress_deadline_seconds": 600,
                "min_ready_seconds": 0
            })
        );
        let rollout = context["rollout"].as_object().expect("a rollout object");
        assert_eq!(rollout.len(), 7);
        assert!(
            rollout.values().all(Value::is_i64),
            "a count is not an integer"
        );
        assert!(!rollout.contains_key("message"));
        // The failure messages name what leaked, not the context.
        let sent = context.to_string();
        assert!(!sent.contains("checkout"), "the Deployment's name was sent");
        assert!(!sent.contains("shop"), "the namespace was sent");
        assert!(
            !sent.contains("ProgressDeadlineExceeded"),
            "the reason was sent"
        );
        assert!(!sent.contains("revision"), "the revision was sent");
    }

    #[test]
    fn an_unknown_count_is_null() {
        let mut record = stalled_rollout();
        (
            record.replicas,
            record.progress_deadline_seconds,
            record.min_ready_seconds,
        ) = (None, None, None);
        let (context, _) = build(&rollout_gathered(Some(record)));
        let rollout = context["rollout"].as_object().expect("a rollout object");
        assert_eq!(rollout.len(), 7);
        for key in ["replicas", "progress_deadline_seconds", "min_ready_seconds"] {
            assert_eq!(rollout[key], Value::Null, "{key}");
        }
        assert!(
            rollout.values().all(|v| v.is_null() || v.is_i64()),
            "a count is neither an integer nor null"
        );
    }

    #[test]
    fn a_stalled_rollout_whose_record_is_gone_sends_a_null_rollout() {
        let (context, _) = build(&rollout_gathered(None));
        let object = context.as_object().expect("a context object");
        assert_eq!(object.get("rollout"), Some(&Value::Null));
    }

    #[test]
    fn no_other_detection_sends_a_rollout() {
        for detection in [Detection::CrashLoop, Detection::PodCreate] {
            let mut g = rollout_gathered(Some(stalled_rollout()));
            g.incident.key.detection = detection;
            g.incident.sample.key.detection = detection;
            let (context, _) = build(&g);
            assert!(context.get("rollout").is_none(), "{detection:?}");
        }
        let mut g = gathered();
        g.rollout = Some(stalled_rollout());
        assert!(build(&g).0.get("rollout").is_none());
    }
}
