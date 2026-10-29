//! The context of an incident as it is sent to the model: only the fields of
//! docs/reference/data-sent-to-the-provider.md (`R001`), every name and free text redacted.

use serde_json::{Value, json};

use crate::incident::Incident;
use crate::record::{EventRecord, PodRecord};
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
    /// Pod: a Job, a workload controller), most recent first.
    pub events: Vec<EventRecord>,
    pub pods_total: usize,
    /// The last lines of the failed container, as read (redacted here).
    pub log_tail: Option<String>,
}

/// Builds the redacted context and returns it with the redactor that holds its pseudonyms.
pub fn build(gathered: &Gathered) -> (Value, Redactor) {
    let mut r = Redactor::new();
    let incident = &gathered.incident;
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
        .map(|pod| pod_context(pod, &mut r))
        .collect();
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
    if let Some(log_tail) = log_tail {
        context["log_tail"] = Value::from(log_tail);
    }
    (context, r)
}

fn pod_context(pod: &PodRecord, r: &mut Redactor) -> Value {
    let containers: Vec<Value> = pod
        .containers
        .iter()
        .map(|c| {
            let probes: Vec<Value> = c
                .probes
                .iter()
                .map(|p| {
                    json!({
                        "kind": p.kind,
                        "initial_delay_seconds": p.initial_delay_seconds,
                        "period_seconds": p.period_seconds,
                        "timeout_seconds": p.timeout_seconds,
                        "failure_threshold": p.failure_threshold,
                    })
                })
                .collect();
            json!({
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
                "memory_limit": c.memory_limit,
                "cpu_limit": c.cpu_limit,
                "probes": probes,
            })
        })
        .collect();
    json!({
        "name": r.name(NameKind::Pod, &pod.name),
        "phase": pod.phase,
        "unschedulable": pod.unschedulable.as_deref().map(|m| r.text(m)),
        "reason": pod.reason.as_deref().map(|m| r.text(m)),
        "message": pod.message.as_deref().map(|m| r.text(m)),
        "containers": containers,
    })
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
    }

    #[test]
    fn the_same_incident_gives_the_same_request() {
        assert_eq!(build(&gathered()).0, build(&gathered()).0);
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
}
