//! The small records kausa keeps instead of whole objects.
//!
//! A watch delivers whole Pods and Events, inline environment values and command lines included.
//! Each one is reduced here, at once, to the few fields the detection rules and (later) the request
//! to the model need; the object itself is dropped and never logged
//! (docs/explanation/architecture.md, "Watching without a large cache").

use std::sync::LazyLock;

use k8s_openapi::api::core::v1::{
    Container, ContainerPort, ContainerStatus, ObjectReference, Pod, PodSpec, PodStatus, Probe,
};
use k8s_openapi::api::events::v1::Event;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use k8s_openapi::jiff::Timestamp;
use regex::Regex;

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

/// The longest resource quantity kept, in bytes. A longer one is dropped, never cut.
pub const MAX_QUANTITY_BYTES: usize = 32;

/// The longest probe path kept once its query and fragment are gone, in bytes. A longer one is
/// dropped whole, never cut, so a cut cannot shorten a token or a hostname below what redaction
/// needs to catch it.
pub const MAX_PROBE_PATH_BYTES: usize = 128;

/// The most `spec.imagePullSecrets` entries counted; only their number is kept, never a name.
pub const MAX_PULL_SECRETS: usize = 16;

/// The grammar of a Kubernetes resource quantity: a decimal number with an exponent, a binary
/// suffix or a decimal suffix.
static QUANTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+|[KMGTPE]i|[numkMGTPE])?$")
        .expect("a valid quantity pattern")
});

/// A quantity as written, if it is at most `MAX_QUANTITY_BYTES` long and matches the quantity
/// grammar; `None` otherwise. The API server returns canonical quantities, so this only keeps
/// free text out of a field the model gets unredacted.
fn quantity(quantity: &Quantity) -> Option<String> {
    let text = quantity.0.as_str();
    (text.len() <= MAX_QUANTITY_BYTES && QUANTITY.is_match(text)).then(|| text.to_owned())
}

/// An HTTP probe's path up to its first `?` or `#`; `None` when that is empty or longer than
/// `MAX_PROBE_PATH_BYTES`. Split at characters, never sliced by byte index.
fn probe_path(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    (!path.is_empty() && path.len() <= MAX_PROBE_PATH_BYTES).then(|| path.to_owned())
}

/// A port number, if it is one (1 to 65535).
fn port_number(port: i32) -> Option<u16> {
    u16::try_from(port).ok().filter(|&port| port != 0)
}

/// A probe's port: a number in range as is, or a name resolved to the `containerPort` of the
/// container's port of that name (never parsed as digits). The name itself is never kept.
fn probe_port(port: &IntOrString, ports: &[ContainerPort]) -> Option<u16> {
    match port {
        IntOrString::Int(number) => port_number(*number),
        IntOrString::String(name) => ports
            .iter()
            .find(|p| p.name.as_deref() == Some(name.as_str()))
            .and_then(|p| port_number(p.container_port)),
    }
}

/// An image pull policy from its closed set, `None` for anything else.
fn image_pull_policy(policy: Option<&str>) -> Option<&'static str> {
    match policy? {
        "Always" => Some("Always"),
        "IfNotPresent" => Some("IfNotPresent"),
        "Never" => Some("Never"),
        _ => None,
    }
}

/// The longest node selector, affinity or toleration key kept, in bytes. An entry with a longer
/// key is dropped and counted, never cut.
pub const MAX_KEY_BYTES: usize = 128;

/// The most entries kept in each of a scheduling record's node selector, node affinity (across
/// all its terms) and toleration lists.
pub const MAX_SCHEDULING_ITEMS: usize = 8;

/// The most required Pod affinity terms kept, and the most required Pod anti-affinity terms.
pub const MAX_AFFINITY_TERMS: usize = 4;

/// The grammar of a Kubernetes qualified name: an optional DNS subdomain prefix and a slash, then
/// a name of 1 to 63 characters.
static QUALIFIED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:[a-z0-9](?:[-a-z0-9]*[a-z0-9])?(?:\.[a-z0-9](?:[-a-z0-9]*[a-z0-9])?)*/)?[A-Za-z0-9](?:[-A-Za-z0-9_.]{0,61}[A-Za-z0-9])?$",
    )
    .expect("a valid qualified-name pattern")
});

/// A label, taint or topology key of an unschedulable Pod, as the record keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// A qualified name, kept as written (and redacted when it is sent).
    Valid(String),
    /// Anything else, kept only as this marker.
    Invalid,
}

/// The record of a key: `None` when it is longer than `MAX_KEY_BYTES` (its entry is dropped and
/// counted), `Key::Invalid` when it is not a qualified name. Never cut.
fn key(text: &str) -> Option<Key> {
    if text.len() > MAX_KEY_BYTES {
        return None;
    }
    Some(if QUALIFIED_NAME.is_match(text) {
        Key::Valid(text.to_owned())
    } else {
        Key::Invalid
    })
}

/// The only node label values kept, with their keys, in the order they are sent. Hypothesis: they
/// are the values the kubelet sets from Go's `GOOS` and `GOARCH`; any other value is only counted.
const CLOSED_VALUES: [(&str, &str); 9] = [
    ("kubernetes.io/os", "linux"),
    ("kubernetes.io/os", "windows"),
    ("kubernetes.io/arch", "amd64"),
    ("kubernetes.io/arch", "arm64"),
    ("kubernetes.io/arch", "arm"),
    ("kubernetes.io/arch", "386"),
    ("kubernetes.io/arch", "ppc64le"),
    ("kubernetes.io/arch", "s390x"),
    ("kubernetes.io/arch", "riscv64"),
];

/// The index in `CLOSED_VALUES` of `value` under the raw `key`, compared exactly.
fn closed_index(key: &str, value: &str) -> Option<usize> {
    CLOSED_VALUES
        .iter()
        .position(|&(closed_key, closed_value)| closed_key == key && closed_value == value)
}

/// A set over `CLOSED_VALUES` (bit `i` is entry `i`): of fixed size, so a requirement that lists a
/// value 100,000 times keeps it once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClosedValues(u16);

impl ClosedValues {
    fn insert(&mut self, index: usize) {
        if index < CLOSED_VALUES.len() {
            self.0 |= 1 << index;
        }
    }

    /// The values in the set, in the order of `CLOSED_VALUES`.
    pub fn iter(self) -> impl Iterator<Item = &'static str> {
        CLOSED_VALUES
            .iter()
            .enumerate()
            .filter(move |&(index, _)| self.0 & (1 << index) != 0)
            .map(|(_, &(_, value))| value)
    }
}

/// A node affinity requirement's operator from its closed set, `other` for anything else.
fn node_operator(operator: &str) -> &'static str {
    match operator {
        "In" => "In",
        "NotIn" => "NotIn",
        "Exists" => "Exists",
        "DoesNotExist" => "DoesNotExist",
        "Gt" => "Gt",
        "Lt" => "Lt",
        _ => "other",
    }
}

/// A toleration's operator: `Equal` when absent (its default), `Exists` or `Equal` as written,
/// `other` for anything else.
fn toleration_operator(operator: Option<&str>) -> &'static str {
    match operator {
        None | Some("Equal") => "Equal",
        Some("Exists") => "Exists",
        Some(_) => "other",
    }
}

/// A toleration's effect: `None` when absent or empty (it tolerates every effect), the effect from
/// its closed set, or `other`.
fn toleration_effect(effect: Option<&str>) -> Option<&'static str> {
    match effect? {
        "" => None,
        "NoSchedule" => Some("NoSchedule"),
        "PreferNoSchedule" => Some("PreferNoSchedule"),
        "NoExecute" => Some("NoExecute"),
        _ => Some("other"),
    }
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PodRecord {
    pub namespace: String,
    pub name: String,
    pub uid: String,
    /// The controller owner reference, if any (a ReplicaSet, StatefulSet, DaemonSet, Job or other).
    pub controller: Option<OwnerRef>,
    pub phase: Option<String>,
    /// The message of the `PodScheduled` condition when the scheduler cannot place the Pod
    /// (status `False`, reason `Unschedulable`, or `SchedulerError` when a plugin such as volume
    /// binding failed), and that reason. A Pod held by its scheduling gates is not recorded.
    pub unschedulable: Option<String>,
    pub unschedulable_reason: Option<String>,
    /// The Pod's `status.reason` and bounded `status.message`, set by the kubelet when it fails the
    /// whole Pod (`Evicted`, `DeadlineExceeded`) or refuses it.
    pub reason: Option<String>,
    pub message: Option<String>,
    /// For a Pod in phase `Failed`, when it failed: the later of its `Ready` condition's last
    /// transition and its containers' finish times. Evicted Pods are not deleted, so this is what
    /// tells a new eviction from one days old.
    pub failed_at: Option<Timestamp>,
    /// The node and service account names, kept only so that redaction can replace them.
    pub node_name: Option<String>,
    pub service_account: Option<String>,
    /// How many `spec.imagePullSecrets` the Pod lists, at most `MAX_PULL_SECRETS`; never a name.
    pub image_pull_secrets: u8,
    /// The scheduling keys of a Pod the scheduler cannot place (set exactly when `unschedulable`
    /// is); `None` for every other Pod, so healthy Pods keep no scheduling data.
    pub scheduling: Option<SchedulingRecord>,
    pub containers: Vec<ContainerRecord>,
}

/// What kausa keeps of an unschedulable Pod's node selector, required affinity terms and
/// tolerations: their keys, operators and effects, and only the closed `os` and `arch` values;
/// never another value, a label selector, a namespace or a toleration's value or seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchedulingRecord {
    /// At most `MAX_SCHEDULING_ITEMS`, in key order.
    pub node_selector: Vec<SelectorEntry>,
    /// The required node affinity's `matchExpressions` then `matchFields` of each term, in term
    /// order, at most `MAX_SCHEDULING_ITEMS` across terms.
    pub node_affinity: Vec<AffinityEntry>,
    /// The required Pod affinity and anti-affinity terms, at most `MAX_AFFINITY_TERMS` each.
    pub pod_affinity: Vec<PodAffinityEntry>,
    pub pod_anti_affinity: Vec<PodAffinityEntry>,
    /// At most `MAX_SCHEDULING_ITEMS`, in spec order.
    pub tolerations: Vec<TolerationEntry>,
    /// How many entries the caps or an over-long key dropped, saturating.
    pub omitted: u16,
}

/// One node selector entry: its key, and its value only if it is a closed `os` or `arch` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectorEntry {
    pub key: Key,
    pub value: Option<&'static str>,
}

/// One required node affinity requirement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffinityEntry {
    /// The index of its term, saturating at 255.
    pub term: u8,
    pub key: Key,
    /// `In`, `NotIn`, `Exists`, `DoesNotExist`, `Gt`, `Lt` or `other`.
    pub operator: &'static str,
    /// The closed `os` and `arch` values it lists, each once.
    pub values: ClosedValues,
    /// How many values it lists, duplicates and closed values included, saturating.
    pub value_count: u16,
}

/// One required Pod affinity or anti-affinity term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodAffinityEntry {
    pub topology_key: Key,
    /// Whether the term lists namespaces or has a namespace selector; never which.
    pub namespaces_set: bool,
}

/// One toleration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TolerationEntry {
    /// `None` for an absent or empty key (it tolerates every taint).
    pub key: Option<Key>,
    /// `Exists`, `Equal` or `other`.
    pub operator: &'static str,
    /// `NoSchedule`, `PreferNoSchedule`, `NoExecute`, `other`, or `None` for every effect.
    pub effect: Option<&'static str>,
}

/// Adds the entry `make` builds to `list` while `list` holds fewer than `cap`; counts it in
/// `omitted` past the cap (without building it) or when `make` drops it for an over-long key.
fn push_capped<T>(
    list: &mut Vec<T>,
    cap: usize,
    omitted: &mut u16,
    make: impl FnOnce() -> Option<T>,
) {
    match (list.len() < cap).then(make).flatten() {
        Some(entry) => list.push(entry),
        None => *omitted = omitted.saturating_add(1),
    }
}

impl SchedulingRecord {
    /// The scheduling keys of `spec`, within their caps. Each list of the spec is walked once, and
    /// an affinity requirement's values without allocating.
    fn from_spec(spec: &PodSpec) -> Self {
        let mut record = Self::default();
        let omitted = &mut record.omitted;

        for (raw, value) in spec.node_selector.iter().flatten() {
            push_capped(
                &mut record.node_selector,
                MAX_SCHEDULING_ITEMS,
                omitted,
                || {
                    Some(SelectorEntry {
                        key: key(raw)?,
                        value: closed_index(raw, value).map(|index| CLOSED_VALUES[index].1),
                    })
                },
            );
        }

        let affinity = spec.affinity.as_ref();
        let terms = affinity
            .and_then(|a| a.node_affinity.as_ref())
            .and_then(|n| {
                n.required_during_scheduling_ignored_during_execution
                    .as_ref()
            })
            .map(|selector| selector.node_selector_terms.as_slice())
            .unwrap_or_default();
        for (index, term) in terms.iter().enumerate() {
            let term_index = u8::try_from(index).unwrap_or(u8::MAX);
            let requirements = term
                .match_expressions
                .iter()
                .flatten()
                .chain(term.match_fields.iter().flatten());
            for requirement in requirements {
                push_capped(
                    &mut record.node_affinity,
                    MAX_SCHEDULING_ITEMS,
                    omitted,
                    || {
                        let key = key(&requirement.key)?;
                        let values = requirement.values.as_deref().unwrap_or_default();
                        let mut closed = ClosedValues::default();
                        for value in values {
                            if let Some(index) = closed_index(&requirement.key, value) {
                                closed.insert(index);
                            }
                        }
                        Some(AffinityEntry {
                            term: term_index,
                            key,
                            operator: node_operator(&requirement.operator),
                            values: closed,
                            value_count: u16::try_from(values.len()).unwrap_or(u16::MAX),
                        })
                    },
                );
            }
        }

        let pod_terms = [
            (
                &mut record.pod_affinity,
                affinity
                    .and_then(|a| a.pod_affinity.as_ref())
                    .and_then(|p| {
                        p.required_during_scheduling_ignored_during_execution
                            .as_ref()
                    }),
            ),
            (
                &mut record.pod_anti_affinity,
                affinity
                    .and_then(|a| a.pod_anti_affinity.as_ref())
                    .and_then(|p| {
                        p.required_during_scheduling_ignored_during_execution
                            .as_ref()
                    }),
            ),
        ];
        for (list, terms) in pod_terms {
            for term in terms.into_iter().flatten() {
                push_capped(list, MAX_AFFINITY_TERMS, omitted, || {
                    Some(PodAffinityEntry {
                        topology_key: key(&term.topology_key)?,
                        namespaces_set: term.namespaces.as_ref().is_some_and(|n| !n.is_empty())
                            || term.namespace_selector.is_some(),
                    })
                });
            }
        }

        for toleration in spec.tolerations.iter().flatten() {
            push_capped(
                &mut record.tolerations,
                MAX_SCHEDULING_ITEMS,
                omitted,
                || {
                    let key = match toleration.key.as_deref() {
                        None | Some("") => None,
                        Some(raw) => Some(key(raw)?),
                    };
                    Some(TolerationEntry {
                        key,
                        operator: toleration_operator(toleration.operator.as_deref()),
                        effect: toleration_effect(toleration.effect.as_deref()),
                    })
                },
            );
        }
        record
    }
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
    /// The memory and CPU requests and limits, each only if it is a valid quantity.
    pub memory_request: Option<String>,
    pub cpu_request: Option<String>,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<String>,
    /// `Always`, `IfNotPresent` or `Never`; `None` for anything else.
    pub image_pull_policy: Option<&'static str>,
    pub probes: Vec<ProbeRecord>,
}

/// The handler and timing of one probe; never its command, headers, host, query or gRPC service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeRecord {
    pub kind: &'static str,
    /// `exec`, `http`, `https`, `tcp` or `grpc`, from the first handler set.
    pub handler: Option<&'static str>,
    /// The port number, a named port resolved against the container's ports.
    pub port: Option<u16>,
    /// An HTTP probe's path without its query and fragment.
    pub path: Option<String>,
    pub initial_delay_seconds: Option<i32>,
    pub period_seconds: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub success_threshold: Option<i32>,
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
        let scheduling_failure = status.and_then(|s| {
            s.conditions.iter().flatten().find(|c| {
                c.type_ == "PodScheduled"
                    && c.status == "False"
                    && matches!(
                        c.reason.as_deref(),
                        Some("Unschedulable" | "SchedulerError")
                    )
            })
        });
        let unschedulable =
            scheduling_failure.map(|c| bounded(c.message.as_deref().unwrap_or_default()));
        let unschedulable_reason = scheduling_failure.and_then(|c| c.reason.clone());
        let phase = status.and_then(|s| s.phase.clone());
        let failed_at = status
            .filter(|_| phase.as_deref() == Some("Failed"))
            .and_then(failed_at);
        Some(Self {
            namespace: metadata.namespace.clone()?,
            name: metadata.name.clone()?,
            uid: metadata.uid.clone()?,
            controller: controller_of(metadata.owner_references.as_ref()),
            phase,
            unschedulable,
            unschedulable_reason,
            reason: status.and_then(|s| s.reason.as_deref().map(bounded)),
            message: status.and_then(|s| s.message.as_deref().map(bounded)),
            failed_at,
            node_name: spec.and_then(|s| s.node_name.clone()),
            service_account: spec.and_then(|s| s.service_account_name.clone()),
            image_pull_secrets: spec
                .and_then(|s| s.image_pull_secrets.as_ref())
                .map_or(0, |secrets| {
                    u8::try_from(secrets.len().min(MAX_PULL_SECRETS)).unwrap_or(u8::MAX)
                }),
            scheduling: scheduling_failure
                .map(|_| spec.map(SchedulingRecord::from_spec).unwrap_or_default()),
            containers,
        })
    }
}

/// The later of the `Ready` condition's last transition and the containers' finish times, if any
/// is set.
fn failed_at(status: &PodStatus) -> Option<Timestamp> {
    let ready = status
        .conditions
        .iter()
        .flatten()
        .filter(|c| c.type_ == "Ready")
        .filter_map(|c| c.last_transition_time.as_ref().map(|t| t.0));
    let finished = [
        status.init_container_statuses.as_ref(),
        status.container_statuses.as_ref(),
    ]
    .into_iter()
    .flatten()
    .flatten()
    .filter_map(|c| c.state.as_ref()?.terminated.as_ref()?.finished_at.as_ref())
    .map(|t| t.0);
    ready.chain(finished).max()
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
        let resources = container.resources.as_ref();
        let requests = resources.and_then(|r| r.requests.as_ref());
        let limits = resources.and_then(|r| r.limits.as_ref());
        let request = |name: &str| requests.and_then(|r| r.get(name)).and_then(quantity);
        let limit = |name: &str| limits.and_then(|l| l.get(name)).and_then(quantity);
        let ports = container.ports.as_deref().unwrap_or_default();
        let probes = [
            ("liveness", container.liveness_probe.as_ref()),
            ("readiness", container.readiness_probe.as_ref()),
            ("startup", container.startup_probe.as_ref()),
        ]
        .into_iter()
        .filter_map(|(kind, probe)| probe.map(|p| ProbeRecord::new(kind, p, ports)))
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
            memory_request: request("memory"),
            cpu_request: request("cpu"),
            memory_limit: limit("memory"),
            cpu_limit: limit("cpu"),
            image_pull_policy: image_pull_policy(container.image_pull_policy.as_deref()),
            probes,
        }
    }
}

impl ProbeRecord {
    /// The record of `probe`, its named port resolved against the container's `ports`. Only the
    /// first handler set is read, in the order `exec`, `httpGet`, `tcpSocket`, `grpc`.
    fn new(kind: &'static str, probe: &Probe, ports: &[ContainerPort]) -> Self {
        let (handler, port, path) = if probe.exec.is_some() {
            (Some("exec"), None, None)
        } else if let Some(http) = &probe.http_get {
            let https = http
                .scheme
                .as_deref()
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("HTTPS"));
            (
                Some(if https { "https" } else { "http" }),
                probe_port(&http.port, ports),
                http.path.as_deref().and_then(probe_path),
            )
        } else if let Some(tcp) = &probe.tcp_socket {
            (Some("tcp"), probe_port(&tcp.port, ports), None)
        } else if let Some(grpc) = &probe.grpc {
            (Some("grpc"), port_number(grpc.port), None)
        } else {
            (None, None, None)
        };
        Self {
            kind,
            handler,
            port,
            path,
            initial_delay_seconds: probe.initial_delay_seconds,
            period_seconds: probe.period_seconds,
            timeout_seconds: probe.timeout_seconds,
            success_threshold: probe.success_threshold,
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

    /// An unschedulable Pod whose spec holds, in every place the record reads or passes by, a
    /// value that must never reach it (docs/adr/0017-richer-context-for-the-model.md).
    fn leaky_pod() -> Pod {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "api-7d9c8b6f4d-2xkqp", "namespace": "shop",
                         "uid": "11111111-1111-4111-8111-111111111111"},
            "spec": {
                "initContainers": [{
                    "name": "warmup",
                    "image": "busybox:1.37",
                    "livenessProbe": {"grpc": {"port": 9090,
                                               "service": "not.a.real.GrpcService"}}
                }],
                "containers": [{
                    "name": "api",
                    "image": "registry.example.com/shop/api:1.4.2",
                    "env": [{"name": "DB_PASSWORD", "value": "not-a-real-password"}],
                    "command": ["/app", "--token=not-a-real-token"],
                    "ports": [{"name": "http", "containerPort": 8080}],
                    "livenessProbe": {
                        "exec": {"command": ["/bin/not-a-real-check", "--not-a-real-exec-flag"]},
                        "terminationGracePeriodSeconds": 4243
                    },
                    "readinessProbe": {"httpGet": {
                        "path": "/ready?probe-query=not-a-real-query-value#not-a-real-fragment",
                        "port": "http",
                        "host": "not-a-real-probe-host.example.net",
                        "httpHeaders": [{"name": "X-Not-A-Real-Header",
                                         "value": "not-a-real-header-value"}]
                    }},
                    "startupProbe": {"tcpSocket": {"port": 8080,
                                                   "host": "not-a-real-tcp-host.example.net"}}
                }],
                "imagePullSecrets": [{"name": "not-a-real-pull-secret"}],
                "nodeSelector": {
                    "example.com/pool": "not-a-real-selector-value",
                    "kubernetes.io/hostname": "not-a-real-hostname-value"
                },
                "affinity": {
                    "nodeAffinity": {
                        "requiredDuringSchedulingIgnoredDuringExecution": {"nodeSelectorTerms": [{
                            "matchExpressions": [
                                {"key": "example.com/zone", "operator": "In",
                                 "values": ["not-a-real-affinity-value"]},
                                {"key": "kubernetes.io/hostname", "operator": "In",
                                 "values": ["not-a-real-affinity-host"]},
                                {"key": "example.com/size", "operator": "Gt",
                                 "values": ["not-a-real-gt-operand"]}
                            ],
                            "matchFields": [{"key": "metadata.name", "operator": "In",
                                             "values": ["not-a-real-field-value"]}]
                        }]},
                        "preferredDuringSchedulingIgnoredDuringExecution": [{
                            "weight": 1,
                            "preference": {"matchExpressions": [{
                                "key": "example.com/not-a-real-preferred-key",
                                "operator": "Exists"
                            }]}
                        }]
                    },
                    "podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
                        "topologyKey": "topology.kubernetes.io/zone",
                        "labelSelector": {"matchLabels": {"app": "not-a-real-label-value"}},
                        "namespaces": ["not-a-real-affinity-namespace"],
                        "matchLabelKeys": ["example.com/not-a-real-match-label-key"]
                    }]},
                    "podAntiAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{
                        "topologyKey": "kubernetes.io/hostname",
                        "namespaceSelector": {"matchLabels": {
                            "team": "not-a-real-namespace-label"
                        }}
                    }]}
                },
                "tolerations": [{"key": "example.com/dedicated", "operator": "Equal",
                                 "value": "not-a-real-toleration-value", "effect": "NoExecute",
                                 "tolerationSeconds": 4242}],
                "topologySpreadConstraints": [{"maxSkew": 1,
                                               "topologyKey": "example.com/not-a-real-spread-key",
                                               "whenUnsatisfiable": "DoNotSchedule"}],
                "schedulerName": "not-a-real-scheduler",
                "priorityClassName": "not-a-real-priority-class"
            },
            "status": {
                "phase": "Pending",
                "conditions": [{
                    "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                    "message": "0/3 nodes are available: 3 node(s) didn't match Pod's node \
                                affinity/selector."
                }]
            }
        }))
        .expect("the fixture is a valid Pod")
    }

    #[test]
    fn the_record_drops_environment_and_command_lines() {
        let record = PodRecord::from_pod(&leaky_pod()).expect("a complete Pod");
        // The fixture is read where 0017 reads: an unschedulable Pod keeps its scheduling, and
        // the probe its path without the query.
        assert!(record.scheduling.is_some(), "no scheduling record");
        let api = record
            .containers
            .iter()
            .find(|c| c.name == "api")
            .expect("the api container");
        let readiness = api
            .probes
            .iter()
            .find(|p| p.kind == "readiness")
            .expect("the readiness probe");
        assert_eq!(readiness.path.as_deref(), Some("/ready"));
        assert_eq!(readiness.port, Some(8080));
        assert_eq!(record.image_pull_secrets, 1);
        // The failure messages name what leaked, not the record: printing a record that holds a
        // secret would log it (CodeQL's rust/cleartext-logging).
        let debug = format!("{record:?}");
        for (leaked, what) in [
            ("not-a-real-password", "the password"),
            ("not-a-real-token", "the token"),
            ("probe-query", "the probe's query"),
            ("not-a-real-query-value", "the probe's query value"),
            ("not-a-real-fragment", "the probe's fragment"),
            ("X-Not-A-Real-Header", "a probe header's name"),
            ("not-a-real-header-value", "a probe header's value"),
            ("not-a-real-check", "the exec command"),
            ("not-a-real-exec-flag", "the exec command's flag"),
            ("4243", "terminationGracePeriodSeconds"),
            ("not-a-real-probe-host", "httpGet.host"),
            ("not-a-real-tcp-host", "tcpSocket.host"),
            ("GrpcService", "the gRPC service"),
            ("not-a-real-pull-secret", "a pull secret's name"),
            ("not-a-real-selector-value", "a node selector value"),
            (
                "not-a-real-hostname-value",
                "a kubernetes.io/hostname selector value",
            ),
            ("not-a-real-affinity-value", "a node affinity value"),
            (
                "not-a-real-affinity-host",
                "a kubernetes.io/hostname affinity value",
            ),
            ("not-a-real-gt-operand", "a Gt operand"),
            ("not-a-real-field-value", "a matchFields value"),
            ("not-a-real-toleration-value", "a toleration value"),
            ("4242", "tolerationSeconds"),
            ("not-a-real-label-value", "a Pod affinity label selector"),
            ("not-a-real-affinity-namespace", "an affinity namespace"),
            (
                "not-a-real-namespace-label",
                "an affinity namespace selector",
            ),
            ("not-a-real-match-label-key", "matchLabelKeys"),
            ("not-a-real-preferred-key", "a preferred term"),
            ("not-a-real-spread-key", "a topology spread constraint"),
            ("not-a-real-scheduler", "the scheduler name"),
            ("not-a-real-priority-class", "the priority class"),
        ] {
            assert!(!debug.contains(leaked), "{what} reached the record");
        }
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
        assert_eq!(
            record.unschedulable_reason.as_deref(),
            Some("Unschedulable")
        );
    }

    #[test]
    fn a_pod_the_scheduler_failed_on_keeps_the_scheduler_message() {
        // Seen on kind for a claim the scheduler could not bind in PreBind.
        let message =
            "running PreBind plugin \"VolumeBinding\": binding volumes: context deadline exceeded";
        let mut pod = crashing_pod();
        let status = pod.status.as_mut().expect("a status");
        status.phase = Some("Pending".into());
        status.container_statuses = None;
        status.conditions = Some(vec![
            serde_json::from_value(json!({
                "type": "PodScheduled", "status": "False", "reason": "SchedulerError",
                "message": message
            }))
            .expect("a condition"),
        ]);
        let record = PodRecord::from_pod(&pod).expect("a complete Pod");
        assert_eq!(record.unschedulable.as_deref(), Some(message));
        assert_eq!(
            record.unschedulable_reason.as_deref(),
            Some("SchedulerError")
        );
        // A Pod held by its scheduling gates waits on purpose.
        let status = pod.status.as_mut().expect("a status");
        status.conditions = Some(vec![
            serde_json::from_value(json!({
                "type": "PodScheduled", "status": "False", "reason": "SchedulingGated",
                "message": "Scheduling is blocked due to non-empty scheduling gates"
            }))
            .expect("a condition"),
        ]);
        let record = PodRecord::from_pod(&pod).expect("a complete Pod");
        assert_eq!(record.unschedulable, None);
        assert_eq!(record.unschedulable_reason, None);
    }

    #[test]
    fn a_failed_pod_keeps_its_reason_message_and_failure_time() {
        let mut pod = crashing_pod();
        // While it runs, a Pod has no failure time.
        let record = PodRecord::from_pod(&pod).expect("a complete Pod");
        assert_eq!(
            (record.reason, record.message, record.failed_at),
            (None, None, None)
        );
        let status = pod.status.as_mut().expect("a status");
        status.phase = Some("Failed".into());
        status.reason = Some("Evicted".into());
        status.message = Some("The node was low on resource: memory. ".into());
        status.conditions = Some(vec![
            serde_json::from_value(json!({
                "type": "Ready", "status": "False", "reason": "PodFailed",
                "lastTransitionTime": "2026-10-28T10:00:00Z"
            }))
            .expect("a condition"),
        ]);
        status.container_statuses = Some(vec![
            serde_json::from_value(json!({
                "name": "api", "image": "registry.example.com/shop/api:1.4.2", "imageID": "",
                "ready": false, "restartCount": 0,
                "state": {"terminated": {"reason": "Error", "exitCode": 137,
                                         "finishedAt": "2026-10-28T10:00:05Z"}}
            }))
            .expect("a container status"),
        ]);
        let record = PodRecord::from_pod(&pod).expect("a complete Pod");
        assert_eq!(record.reason.as_deref(), Some("Evicted"));
        assert_eq!(
            record.message.as_deref(),
            Some("The node was low on resource: memory. ")
        );
        // The later of the two times.
        assert_eq!(
            record.failed_at,
            Some("2026-10-28T10:00:05Z".parse().expect("a timestamp"))
        );
    }

    #[test]
    fn a_pod_failed_during_its_init_containers_takes_their_finish_time() {
        // Evicted before its main container started: only the init container has terminated, and
        // the Ready condition has been False since the Pod was created.
        let mut pod = crashing_pod();
        let status = pod.status.as_mut().expect("a status");
        status.phase = Some("Failed".into());
        status.reason = Some("Evicted".into());
        status.conditions = Some(vec![
            serde_json::from_value(json!({
                "type": "Ready", "status": "False", "reason": "PodFailed",
                "lastTransitionTime": "2026-10-28T09:00:00Z"
            }))
            .expect("a condition"),
        ]);
        status.init_container_statuses = Some(vec![
            serde_json::from_value(json!({
                "name": "migrate", "image": "busybox:1.37", "imageID": "",
                "ready": false, "restartCount": 0,
                "state": {"terminated": {"reason": "Error", "exitCode": 137,
                                         "finishedAt": "2026-10-28T10:00:05Z"}}
            }))
            .expect("an init container status"),
        ]);
        status.container_statuses = Some(vec![
            serde_json::from_value(json!({
                "name": "api", "image": "registry.example.com/shop/api:1.4.2", "imageID": "",
                "ready": false, "restartCount": 0,
                "state": {"waiting": {"reason": "PodInitializing"}}
            }))
            .expect("a container status"),
        ]);
        let record = PodRecord::from_pod(&pod).expect("a complete Pod");
        assert_eq!(
            record.failed_at,
            Some("2026-10-28T10:00:05Z".parse().expect("a timestamp"))
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
    fn the_namespaces_are_compared_exactly() {
        for namespace in ["Shop", "SHOP", " shop", "shop ", " shop ", "shop\n"] {
            let mut event = backoff_event();
            event.metadata.namespace = Some(namespace.into());
            assert_eq!(
                EventRecord::from_event(&event),
                None,
                "event in {namespace:?}"
            );

            let mut event = backoff_event();
            event
                .regarding
                .as_mut()
                .expect("a regarding object")
                .namespace = Some(namespace.into());
            assert_eq!(
                EventRecord::from_event(&event),
                None,
                "object in {namespace:?}"
            );
        }
    }

    #[test]
    fn an_event_about_a_cluster_scoped_object_is_skipped() {
        fn node_event(event_namespace: &str, object_namespace: Option<&str>) -> Event {
            let mut event = backoff_event();
            event.metadata.namespace = Some(event_namespace.into());
            let regarding = event.regarding.as_mut().expect("a regarding object");
            regarding.kind = Some("Node".into());
            regarding.namespace = object_namespace.map(Into::into);
            event
        }

        // No namespace on the object: dropped by the parsing of `regarding` too.
        assert_eq!(EventRecord::from_event(&node_event("default", None)), None);
        // An empty one parses; only the comparison with the event's namespace drops it.
        assert_eq!(
            EventRecord::from_event(&node_event("default", Some(""))),
            None
        );
        // Both empty compare equal; only the check for a non-empty namespace drops it.
        assert_eq!(EventRecord::from_event(&node_event("", Some(""))), None);
        // The kind plays no part: the same event about an object in its namespace is kept.
        let record = EventRecord::from_event(&node_event("default", Some("default")))
            .expect("an event in the namespace of its object");
        assert_eq!(record.regarding.kind, "Node");
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

    // Acceptance tests for docs/adr/0017-richer-context-for-the-model.md, written before the
    // code. They use the constants `MAX_QUANTITY_BYTES`, `MAX_PROBE_PATH_BYTES`, `MAX_KEY_BYTES`,
    // `MAX_SCHEDULING_ITEMS`, `MAX_AFFINITY_TERMS` and `MAX_PULL_SECRETS`; the new fields of
    // `ContainerRecord` (`memory_request`, `cpu_request`, `image_pull_policy`), `ProbeRecord`
    // (`handler`, `port`, `path`, `success_threshold`) and `PodRecord` (`image_pull_secrets`,
    // `scheduling`); `SchedulingRecord` and its entries; `Key`, `key()`, `CLOSED_VALUES`,
    // `closed_index()` and `ClosedValues::iter()`. Values are invented.

    use serde_json::Value;

    /// The record of a Pod with `spec`. With a reason, the scheduler failed to place it
    /// (`PodScheduled` is `False` with that reason); without one, it is scheduled and running.
    fn record_of(spec: Value, reason: Option<&str>) -> PodRecord {
        let status = match reason {
            Some(reason) => json!({
                "phase": "Pending",
                "conditions": [{"type": "PodScheduled", "status": "False", "reason": reason,
                                "message": "0/3 nodes are available"}]
            }),
            None => json!({
                "phase": "Running",
                "conditions": [{"type": "PodScheduled", "status": "True"}]
            }),
        };
        let pod: Pod = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "web-5c6d7e8f9a-q4w5e", "namespace": "shop",
                         "uid": "55555555-5555-4555-8555-555555555555"},
            "spec": spec,
            "status": status
        }))
        .expect("the fixture is a valid Pod");
        PodRecord::from_pod(&pod).expect("a complete Pod")
    }

    /// A container `web` with `fields` added to its spec.
    fn container(fields: Value) -> Value {
        let mut container = json!({"name": "web", "image": "busybox:1.37"});
        let object = container.as_object_mut().expect("an object");
        for (name, value) in fields.as_object().expect("the fields are an object") {
            object.insert(name.clone(), value.clone());
        }
        container
    }

    /// A Pod with one container `web` with `fields`, and `extra` added to its spec.
    fn spec_with(fields: Value, extra: Value) -> Value {
        let mut spec = json!({"containers": [container(fields)]});
        let object = spec.as_object_mut().expect("an object");
        for (name, value) in extra.as_object().expect("the extra fields are an object") {
            object.insert(name.clone(), value.clone());
        }
        spec
    }

    /// The record of `probe` as the readiness probe of a container whose ports are `http` (8080),
    /// `huge` (70000, out of range) and `zero` (0).
    fn readiness(probe: Value) -> ProbeRecord {
        let record = record_of(
            spec_with(
                json!({
                    "ports": [{"name": "http", "containerPort": 8080},
                              {"name": "huge", "containerPort": 70000},
                              {"name": "zero", "containerPort": 0}],
                    "readinessProbe": probe
                }),
                json!({}),
            ),
            None,
        );
        record.containers[0].probes[0].clone()
    }

    /// The scheduling record of an unschedulable Pod with `extra` in its spec.
    fn scheduling_of(extra: Value) -> SchedulingRecord {
        record_of(spec_with(json!({}), extra), Some("Unschedulable"))
            .scheduling
            .expect("an unschedulable Pod keeps its scheduling")
    }

    /// A key as a string, or `<invalid>`.
    fn shown(key: &Key) -> &str {
        match key {
            Key::Valid(key) => key,
            Key::Invalid => "<invalid>",
        }
    }

    #[test]
    fn requests_are_recorded_and_checked_like_limits() {
        assert_eq!(MAX_QUANTITY_BYTES, 32);
        type Quantities = (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let resources = |requests: Value, limits: Value| -> Quantities {
            let record = record_of(
                spec_with(
                    json!({"resources": {"requests": requests, "limits": limits}}),
                    json!({}),
                ),
                None,
            );
            let c = &record.containers[0];
            (
                c.memory_request.clone(),
                c.cpu_request.clone(),
                c.memory_limit.clone(),
                c.cpu_limit.clone(),
            )
        };
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(
            resources(
                json!({"memory": "128Mi", "cpu": "250m"}),
                json!({"memory": "256Mi", "cpu": "500m"})
            ),
            (some("128Mi"), some("250m"), some("256Mi"), some("500m"))
        );
        // A container written with limits only, as the API server returns it (fact 1 of the
        // ADR): with requests equal to its limits, in canonical form.
        assert_eq!(
            resources(
                json!({"memory": "67108864", "cpu": "500m"}),
                json!({"memory": "67108864", "cpu": "500m"})
            ),
            (
                some("67108864"),
                some("500m"),
                some("67108864"),
                some("500m")
            )
        );
        // No resources at all.
        let record = record_of(spec_with(json!({}), json!({})), None);
        assert_eq!(
            (
                &record.containers[0].memory_request,
                &record.containers[0].cpu_request
            ),
            (&None, &None)
        );

        let longest = "1".repeat(MAX_QUANTITY_BYTES);
        for good in [
            "1",
            "0",
            "0.5",
            ".5",
            "1.",
            "1e3",
            "1E-3",
            "+1",
            "-1",
            "128Mi",
            "1Ki",
            "2Ei",
            "100n",
            "100u",
            "250m",
            "1k",
            "1M",
            "1G",
            "1T",
            "1P",
            "1E",
            "1.5Gi",
            longest.as_str(),
        ] {
            assert_eq!(
                resources(
                    json!({"memory": good, "cpu": good}),
                    json!({"memory": good, "cpu": good})
                ),
                (some(good), some(good), some(good), some(good)),
                "{good} is a quantity"
            );
        }
        let too_long = "1".repeat(MAX_QUANTITY_BYTES + 1);
        for bad in [
            too_long.as_str(),
            "12 GB",
            "",
            "Mi",
            "1Mi ",
            " 1Mi",
            "1mi",
            "1K",
            "1KiB",
            "0x10",
            "1e3Mi",
            "1,5",
            "1..5",
            "e3",
            "1Mi\n",
        ] {
            assert_eq!(
                resources(
                    json!({"memory": bad, "cpu": bad}),
                    json!({"memory": bad, "cpu": bad})
                ),
                (None, None, None, None),
                "{bad:?} is not a quantity"
            );
        }

        // Init containers carry their requests too.
        let record = record_of(
            json!({
                "initContainers": [{"name": "migrate", "image": "busybox:1.37",
                                    "resources": {"requests": {"memory": "64Mi", "cpu": "100m"}}}],
                "containers": [container(json!({}))]
            }),
            None,
        );
        let migrate = &record.containers[0];
        assert!(migrate.init);
        assert_eq!(migrate.memory_request.as_deref(), Some("64Mi"));
        assert_eq!(migrate.cpu_request.as_deref(), Some("100m"));
    }

    #[test]
    fn each_probe_handler_is_recorded() {
        let http = json!({"path": "/ready", "port": 8080});
        let with_scheme =
            |scheme: &str| json!({"httpGet": {"path": "/ready", "port": 8080, "scheme": scheme}});
        for (probe, handler) in [
            (json!({"exec": {"command": ["true"]}}), Some("exec")),
            (json!({"httpGet": http}), Some("http")),
            (with_scheme("HTTP"), Some("http")),
            (with_scheme("HTTPS"), Some("https")),
            (with_scheme("https"), Some("https")),
            (with_scheme("hTtPs"), Some("https")),
            (with_scheme("HTTPS2"), Some("http")),
            (json!({"tcpSocket": {"port": 8080}}), Some("tcp")),
            (json!({"grpc": {"port": 9090}}), Some("grpc")),
            (json!({"periodSeconds": 10}), None),
            // The first that is set, in the order exec, httpGet, tcpSocket, grpc.
            (
                json!({"exec": {"command": ["true"]}, "httpGet": http}),
                Some("exec"),
            ),
            (
                json!({"httpGet": http, "tcpSocket": {"port": 8080}}),
                Some("http"),
            ),
            (
                json!({"tcpSocket": {"port": 8080}, "grpc": {"port": 9090}}),
                Some("tcp"),
            ),
        ] {
            assert_eq!(readiness(probe.clone()).handler, handler, "{probe}");
        }

        // The whole record of a probe: its handler, port, path and timing, and nothing else.
        assert_eq!(
            readiness(json!({
                "httpGet": {"path": "/ready", "port": "http", "scheme": "HTTP"},
                "initialDelaySeconds": 5, "periodSeconds": 10, "timeoutSeconds": 1,
                "successThreshold": 2, "failureThreshold": 3
            })),
            ProbeRecord {
                kind: "readiness",
                handler: Some("http"),
                port: Some(8080),
                path: Some("/ready".into()),
                initial_delay_seconds: Some(5),
                period_seconds: Some(10),
                timeout_seconds: Some(1),
                success_threshold: Some(2),
                failure_threshold: Some(3),
            }
        );
    }

    #[test]
    fn probe_ports_resolve_names_and_drop_the_rest() {
        let http = |port: Value| json!({"httpGet": {"path": "/ready", "port": port}});
        let tcp = |port: Value| json!({"tcpSocket": {"port": port}});
        let grpc = |port: i64| json!({"grpc": {"port": port}});
        for (probe, port) in [
            (http(json!(8080)), Some(8080)),
            (http(json!("http")), Some(8080)),
            (tcp(json!(8080)), Some(8080)),
            (tcp(json!("http")), Some(8080)),
            (tcp(json!(1)), Some(1)),
            (tcp(json!(65535)), Some(65535)),
            (grpc(9090), Some(9090)),
            // A name that matches no port, a name made of digits (never parsed), and a name
            // whose port is out of range.
            (http(json!("nosuchport")), None),
            (tcp(json!("nosuchport")), None),
            (http(json!("8080")), None),
            (http(json!("huge")), None),
            (tcp(json!("zero")), None),
            // Numbers out of range.
            (http(json!(0)), None),
            (http(json!(70000)), None),
            (http(json!(-1)), None),
            (tcp(json!(0)), None),
            (tcp(json!(70000)), None),
            (grpc(0), None),
            (grpc(70000), None),
            // No port at all.
            (json!({"exec": {"command": ["true"]}}), None),
            (json!({"periodSeconds": 10}), None),
        ] {
            assert_eq!(readiness(probe.clone()).port, port, "{probe}");
        }
    }

    #[test]
    fn the_probe_path_loses_its_query_and_fragment() {
        assert_eq!(MAX_PROBE_PATH_BYTES, 128);
        let path = |path: &str| readiness(json!({"httpGet": {"path": path, "port": 8080}})).path;
        assert_eq!(path("/ready?token=x#f"), Some("/ready".into()));
        assert_eq!(path("/ready#f?token=x"), Some("/ready".into()));
        assert_eq!(path("/ready"), Some("/ready".into()));
        assert_eq!(
            path("/api/v1/health.json"),
            Some("/api/v1/health.json".into())
        );
        assert_eq!(path("/é/ready?x=é"), Some("/é/ready".into()));
        // Never cut: at most 128 bytes once the query is gone, or nothing.
        let longest = format!("/{}", "a".repeat(MAX_PROBE_PATH_BYTES - 1));
        assert_eq!(path(&longest), Some(longest.clone()));
        assert_eq!(
            path(&format!("{longest}?{}", "q".repeat(200))),
            Some(longest)
        );
        assert_eq!(
            path(&format!("/{}", "a".repeat(MAX_PROBE_PATH_BYTES))),
            None
        );
        // 129 bytes of two-byte characters: dropped whole, never cut inside one.
        assert_eq!(path(&format!("/{}", "é".repeat(64))), None);
        assert_eq!(path(""), None);
        assert_eq!(path("?only"), None);
        assert_eq!(path("#only"), None);
        // An HTTP probe without a path, and the other handlers, have none.
        assert_eq!(readiness(json!({"httpGet": {"port": 8080}})).path, None);
        assert_eq!(readiness(json!({"tcpSocket": {"port": 8080}})).path, None);
        assert_eq!(readiness(json!({"grpc": {"port": 9090}})).path, None);
        assert_eq!(
            readiness(json!({"exec": {"command": ["/ready"]}})).path,
            None
        );
    }

    #[test]
    fn pull_settings_are_mapped_and_counted() {
        assert_eq!(MAX_PULL_SECRETS, 16);
        let policy = |policy: Option<&str>| {
            let fields = match policy {
                Some(policy) => json!({"imagePullPolicy": policy}),
                None => json!({}),
            };
            record_of(spec_with(fields, json!({})), None).containers[0].image_pull_policy
        };
        for kept in ["Always", "IfNotPresent", "Never"] {
            assert_eq!(policy(Some(kept)), Some(kept));
        }
        for unknown in ["always", "Sometimes", "", "IfNotPresent "] {
            assert_eq!(policy(Some(unknown)), None, "{unknown:?}");
        }
        assert_eq!(policy(None), None);
        // Init containers carry theirs too.
        let record = record_of(
            json!({
                "initContainers": [{"name": "migrate", "image": "busybox:1.37",
                                    "imagePullPolicy": "Never"}],
                "containers": [container(json!({}))]
            }),
            None,
        );
        assert_eq!(record.containers[0].image_pull_policy, Some("Never"));

        let secrets = |count: usize| {
            let secrets: Vec<Value> = (0..count)
                .map(|i| json!({"name": format!("pull-{i}")}))
                .collect();
            record_of(
                spec_with(json!({}), json!({"imagePullSecrets": secrets})),
                None,
            )
            .image_pull_secrets
        };
        assert_eq!(
            record_of(spec_with(json!({}), json!({})), None).image_pull_secrets,
            0
        );
        for (count, kept) in [
            (0, 0),
            (1, 1),
            (3, 3),
            (16, 16),
            (17, 16),
            (20, 16),
            (300, 16),
        ] {
            assert_eq!(secrets(count), kept, "{count} pull secrets");
        }
    }

    #[test]
    fn scheduling_is_kept_only_for_an_unschedulable_pod() {
        let spec = spec_with(json!({}), json!({"nodeSelector": {"disktype": "ssd"}}));
        for reason in ["Unschedulable", "SchedulerError"] {
            let scheduling = record_of(spec.clone(), Some(reason))
                .scheduling
                .unwrap_or_else(|| panic!("no scheduling for {reason}"));
            assert_eq!(scheduling.node_selector.len(), 1);
            assert_eq!(
                scheduling.node_selector[0].key,
                Key::Valid("disktype".into())
            );
            assert_eq!(scheduling.node_selector[0].value, None);
        }
        // A running Pod, a Pod held by its scheduling gates and a Pending Pod the scheduler has
        // not judged yet keep none.
        assert_eq!(record_of(spec.clone(), None).scheduling, None);
        assert_eq!(
            record_of(spec.clone(), Some("SchedulingGated")).scheduling,
            None
        );
        let pod: Pod = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "web-5c6d7e8f9a-q4w5e", "namespace": "shop",
                         "uid": "55555555-5555-4555-8555-555555555555"},
            "spec": spec,
            "status": {"phase": "Pending"}
        }))
        .expect("the fixture is a valid Pod");
        assert_eq!(
            PodRecord::from_pod(&pod)
                .expect("a complete Pod")
                .scheduling,
            None
        );
        // An unschedulable Pod without a spec keeps an empty record.
        let pod: Pod = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "web-5c6d7e8f9a-q4w5e", "namespace": "shop",
                         "uid": "55555555-5555-4555-8555-555555555555"},
            "status": {"phase": "Pending",
                       "conditions": [{"type": "PodScheduled", "status": "False",
                                       "reason": "Unschedulable",
                                       "message": "0/3 nodes are available"}]}
        }))
        .expect("the fixture is a valid Pod");
        assert_eq!(
            PodRecord::from_pod(&pod)
                .expect("a complete Pod")
                .scheduling,
            Some(SchedulingRecord::default())
        );
    }

    #[test]
    fn scheduling_lists_are_capped_and_counted() {
        assert_eq!((MAX_SCHEDULING_ITEMS, MAX_AFFINITY_TERMS), (8, 4));
        let selector: serde_json::Map<String, Value> = (0..10)
            .map(|i| (format!("example.com/selector-{i:02}"), json!("v")))
            .collect();
        let expressions = |range: std::ops::Range<usize>| -> Vec<Value> {
            range
                .map(|i| json!({"key": format!("example.com/affinity-{i:02}"), "operator": "Exists"}))
                .collect()
        };
        let terms = |count: usize, name: &str| -> Vec<Value> {
            (0..count)
                .map(|i| json!({"topologyKey": format!("example.com/{name}-{i}")}))
                .collect()
        };
        let tolerations: Vec<Value> = (0..11)
            .map(|i| json!({"key": format!("example.com/taint-{i:02}"), "operator": "Exists"}))
            .collect();
        let scheduling = scheduling_of(json!({
            "nodeSelector": selector,
            "affinity": {
                "nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                    "nodeSelectorTerms": [
                        {"matchExpressions": expressions(0..5),
                         "matchFields": [{"key": "metadata.name", "operator": "In",
                                          "values": ["node-a"]}]},
                        {"matchExpressions": expressions(5..10)}
                    ]
                }},
                "podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution":
                                terms(6, "affinity")},
                "podAntiAffinity": {"requiredDuringSchedulingIgnoredDuringExecution":
                                    terms(5, "anti")}
            },
            "tolerations": tolerations
        }));

        let selector: Vec<&str> = scheduling
            .node_selector
            .iter()
            .map(|e| shown(&e.key))
            .collect();
        let expected: Vec<String> = (0..8)
            .map(|i| format!("example.com/selector-{i:02}"))
            .collect();
        assert_eq!(selector, expected);

        // matchExpressions then matchFields of each term, in term order, 8 across terms.
        let affinity: Vec<(u8, &str)> = scheduling
            .node_affinity
            .iter()
            .map(|e| (e.term, shown(&e.key)))
            .collect();
        assert_eq!(
            affinity,
            [
                (0, "example.com/affinity-00"),
                (0, "example.com/affinity-01"),
                (0, "example.com/affinity-02"),
                (0, "example.com/affinity-03"),
                (0, "example.com/affinity-04"),
                (0, "metadata.name"),
                (1, "example.com/affinity-05"),
                (1, "example.com/affinity-06"),
            ]
        );
        let topology = |entries: &[PodAffinityEntry]| -> Vec<String> {
            entries
                .iter()
                .map(|e| shown(&e.topology_key).to_owned())
                .collect()
        };
        assert_eq!(
            topology(&scheduling.pod_affinity),
            (0..4)
                .map(|i| format!("example.com/affinity-{i}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            topology(&scheduling.pod_anti_affinity),
            (0..4)
                .map(|i| format!("example.com/anti-{i}"))
                .collect::<Vec<_>>()
        );
        let tolerations: Vec<&str> = scheduling
            .tolerations
            .iter()
            .map(|t| t.key.as_ref().map_or("<none>", shown))
            .collect();
        let expected: Vec<String> = (0..8)
            .map(|i| format!("example.com/taint-{i:02}"))
            .collect();
        assert_eq!(tolerations, expected);
        // 2 selector entries, 3 affinity expressions, 2 affinity and 1 anti-affinity terms and
        // 3 tolerations past their caps.
        assert_eq!(scheduling.omitted, 2 + 3 + 2 + 1 + 3);

        // The count saturates.
        let tolerations: Vec<Value> = (0..70_000)
            .map(|_| json!({"key": "example.com/taint", "operator": "Exists"}))
            .collect();
        let scheduling = scheduling_of(json!({"tolerations": tolerations}));
        assert_eq!(scheduling.tolerations.len(), MAX_SCHEDULING_ITEMS);
        assert_eq!(scheduling.omitted, u16::MAX);
    }

    #[test]
    fn a_long_key_is_dropped_and_an_invalid_one_marked() {
        assert_eq!(MAX_KEY_BYTES, 128);
        let longest = format!("{}.example.com/{}", "a".repeat(52), "b".repeat(63));
        let too_long = format!("{}.example.com/{}", "a".repeat(53), "b".repeat(63));
        assert_eq!((longest.len(), too_long.len()), (128, 129));
        assert_eq!(key(&too_long), None);
        assert_eq!(key(&"bad key".repeat(20)), None);
        assert_eq!(key(&longest), Some(Key::Valid(longest.clone())));
        for valid in [
            "disktype",
            "a",
            "Pool_1.a",
            "topology.kubernetes.io/zone",
            "kubernetes.io/os",
            "example.com/pool",
            "eks.amazonaws.com/nodegroup",
            "node-role.kubernetes.io/control-plane",
        ] {
            assert_eq!(key(valid), Some(Key::Valid(valid.into())), "{valid}");
        }
        let long_name = format!("example.com/{}", "b".repeat(64));
        for invalid in [
            "bad key",
            "",
            "Example.com/pool",
            "example.com/",
            "/pool",
            "-pool",
            "pool-",
            "a/b/c",
            "example..com/pool",
            "pool\n",
            "é",
            long_name.as_str(),
        ] {
            assert_eq!(key(invalid), Some(Key::Invalid), "{invalid:?}");
        }

        // In a Pod: an over-long key drops its entry and counts it; an invalid one is marked.
        let scheduling = scheduling_of(json!({
            "nodeSelector": {too_long.clone(): "v", "bad key": "v", "disktype": "v"},
            "affinity": {
                "nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                    "nodeSelectorTerms": [{"matchExpressions": [
                        {"key": too_long.clone(), "operator": "Exists"},
                        {"key": "bad key", "operator": "Exists"}
                    ]}]
                }},
                "podAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [
                    {"topologyKey": too_long.clone()}
                ]},
                "podAntiAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [
                    {"topologyKey": "bad key"}
                ]}
            },
            "tolerations": [
                {"key": too_long.clone(), "operator": "Exists"},
                {"key": "bad key", "operator": "Exists"}
            ]
        }));
        let selector: Vec<&Key> = scheduling.node_selector.iter().map(|e| &e.key).collect();
        assert_eq!(selector, [&Key::Invalid, &Key::Valid("disktype".into())]);
        assert_eq!(scheduling.node_affinity.len(), 1);
        assert_eq!(scheduling.node_affinity[0].key, Key::Invalid);
        assert!(scheduling.pod_affinity.is_empty());
        assert_eq!(scheduling.pod_anti_affinity.len(), 1);
        assert_eq!(scheduling.pod_anti_affinity[0].topology_key, Key::Invalid);
        assert_eq!(scheduling.tolerations.len(), 1);
        assert_eq!(scheduling.tolerations[0].key, Some(Key::Invalid));
        assert_eq!(scheduling.omitted, 4);
        assert!(
            !format!("{scheduling:?}").contains(&too_long),
            "an over-long key reached the record"
        );
    }

    #[test]
    fn operators_and_effects_map_to_closed_sets() {
        let expressions: Vec<Value> = [
            "In",
            "NotIn",
            "Exists",
            "DoesNotExist",
            "Gt",
            "Lt",
            "Matches",
            "in",
        ]
        .iter()
        .map(|operator| json!({"key": "example.com/pool", "operator": operator}))
        .collect();
        let scheduling = scheduling_of(json!({
            "affinity": {"nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                "nodeSelectorTerms": [{"matchExpressions": expressions}]
            }}},
            "tolerations": [
                {"key": "example.com/a", "effect": "NoSchedule"},
                {"key": "example.com/b", "operator": "Exists", "effect": "PreferNoSchedule"},
                {"key": "example.com/c", "operator": "Equal", "value": "v", "effect": "NoExecute"},
                {"key": "example.com/d", "operator": "Contains", "effect": ""},
                {"key": "example.com/e", "operator": "exists"},
                {"key": "example.com/f", "operator": "Exists", "effect": "Evict"},
                {"operator": "Exists"},
                {"key": "", "operator": "Exists"}
            ]
        }));
        let operators: Vec<&str> = scheduling
            .node_affinity
            .iter()
            .map(|e| e.operator)
            .collect();
        assert_eq!(
            operators,
            [
                "In",
                "NotIn",
                "Exists",
                "DoesNotExist",
                "Gt",
                "Lt",
                "other",
                "other"
            ]
        );
        let tolerations: Vec<(Option<&str>, &str, Option<&str>)> = scheduling
            .tolerations
            .iter()
            .map(|t| (t.key.as_ref().map(shown), t.operator, t.effect))
            .collect();
        assert_eq!(
            tolerations,
            [
                (Some("example.com/a"), "Equal", Some("NoSchedule")),
                (Some("example.com/b"), "Exists", Some("PreferNoSchedule")),
                (Some("example.com/c"), "Equal", Some("NoExecute")),
                (Some("example.com/d"), "other", None),
                (Some("example.com/e"), "other", None),
                (Some("example.com/f"), "Exists", Some("other")),
                (None, "Exists", None),
                (None, "Exists", None),
            ]
        );
        assert_eq!(scheduling.omitted, 0);
    }

    #[test]
    fn only_closed_os_and_arch_values_are_kept() {
        assert_eq!(
            CLOSED_VALUES,
            [
                ("kubernetes.io/os", "linux"),
                ("kubernetes.io/os", "windows"),
                ("kubernetes.io/arch", "amd64"),
                ("kubernetes.io/arch", "arm64"),
                ("kubernetes.io/arch", "arm"),
                ("kubernetes.io/arch", "386"),
                ("kubernetes.io/arch", "ppc64le"),
                ("kubernetes.io/arch", "s390x"),
                ("kubernetes.io/arch", "riscv64"),
            ]
        );
        assert_eq!(closed_index("kubernetes.io/os", "linux"), Some(0));
        assert_eq!(closed_index("kubernetes.io/arch", "amd64"), Some(2));
        assert_eq!(closed_index("kubernetes.io/arch", "riscv64"), Some(8));
        for (key, value) in [
            ("kubernetes.io/os", "amd64"),
            ("kubernetes.io/arch", "linux"),
            ("kubernetes.io/os", "Linux"),
            ("kubernetes.io/arch", "AMD64"),
            ("kubernetes.io/os", "plan9"),
            ("beta.kubernetes.io/arch", "amd64"),
            ("example.com/os", "linux"),
            ("kubernetes.io/hostname", "linux"),
        ] {
            assert_eq!(closed_index(key, value), None, "{key}={value}");
        }

        let scheduling = scheduling_of(json!({
            "nodeSelector": {
                "kubernetes.io/os": "linux",
                "kubernetes.io/arch": "arm64",
                "example.com/pool": "linux",
                "kubernetes.io/hostname": "node-a",
                "topology.kubernetes.io/zone": "zone-a"
            },
            "affinity": {"nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                "nodeSelectorTerms": [{"matchExpressions": [
                    {"key": "kubernetes.io/os", "operator": "In",
                     "values": ["windows", "linux", "freebsd"]},
                    {"key": "kubernetes.io/arch", "operator": "NotIn", "values": ["sparc", "arm"]},
                    {"key": "beta.kubernetes.io/arch", "operator": "In", "values": ["amd64"]},
                    {"key": "kubernetes.io/hostname", "operator": "In",
                     "values": ["node-a", "node-b"]},
                    {"key": "example.com/size", "operator": "Gt", "values": ["4"]},
                    {"key": "kubernetes.io/os", "operator": "Exists"},
                    {"key": "kubernetes.io/os", "operator": "In", "values": ["amd64"]}
                ]}]
            }}}
        }));
        let selector: Vec<(&str, Option<&str>)> = scheduling
            .node_selector
            .iter()
            .map(|e| (shown(&e.key), e.value))
            .collect();
        assert_eq!(
            selector,
            [
                ("example.com/pool", None),
                ("kubernetes.io/arch", Some("arm64")),
                ("kubernetes.io/hostname", None),
                ("kubernetes.io/os", Some("linux")),
                ("topology.kubernetes.io/zone", None),
            ]
        );
        let affinity: Vec<(Vec<&str>, u16)> = scheduling
            .node_affinity
            .iter()
            .map(|e| (e.values.iter().collect(), e.value_count))
            .collect();
        assert_eq!(
            affinity,
            [
                (vec!["linux", "windows"], 3),
                (vec!["arm"], 2),
                (vec![], 1),
                (vec![], 2),
                (vec![], 1),
                (vec![], 0),
                (vec![], 1),
            ]
        );

        let scheduling = scheduling_of(json!({
            "nodeSelector": {"kubernetes.io/os": "plan9", "kubernetes.io/arch": "AMD64"}
        }));
        assert!(scheduling.node_selector.iter().all(|e| e.value.is_none()));
    }

    #[test]
    fn a_long_affinity_value_list_keeps_each_closed_value_once() {
        // A fixed size whatever the list holds.
        assert_eq!(std::mem::size_of::<ClosedValues>(), 2);
        let entry = |values: Vec<&str>| {
            scheduling_of(json!({
                "affinity": {"nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                    "nodeSelectorTerms": [{"matchExpressions": [
                        {"key": "kubernetes.io/arch", "operator": "In", "values": values}
                    ]}]
                }}}
            }))
            .node_affinity[0]
                .clone()
        };
        let many = entry(vec!["amd64"; 100_000]);
        assert_eq!(many.values.iter().collect::<Vec<_>>(), ["amd64"]);
        assert_eq!(many.value_count, u16::MAX);

        // Every closed value, backwards, and values outside the list: each arch value once, in
        // the order of the closed list; the os values do not belong to this key.
        let mut values: Vec<&str> = CLOSED_VALUES.iter().rev().map(|(_, v)| *v).collect();
        values.extend(["sparc", "mips", "AMD64", "amd64", "arm64"]);
        let mixed = entry(values);
        assert_eq!(
            mixed.values.iter().collect::<Vec<_>>(),
            [
                "amd64", "arm64", "arm", "386", "ppc64le", "s390x", "riscv64"
            ]
        );
        assert_eq!(mixed.value_count, 14);
        assert_eq!(ClosedValues::default().iter().count(), 0);
    }
}
