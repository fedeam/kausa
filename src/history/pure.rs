//! The pure part of the explanation history (docs/adr/0014-explanation-history.md): no I/O and no
//! clock; times come in as strings. The fuzz crate includes this file by path.
//!
//! Times are always `YYYY-MM-DDTHH:MM:SSZ` (UTC, whole seconds), so they compare as strings.
//! Everything read back from a stored object is untrusted (invariant 5): it is checked field by
//! field, an entry that does not pass is dropped, nothing panics, and no message built here quotes
//! it.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use serde_json::{Value, json};

use crate::prompt::{
    self, Explanation, MAX_CAUSE, MAX_STEP, MAX_STEPS, MAX_SUMMARY, PROMPT_VERSION,
};

/// The prefix of an Explanation's name, before the fingerprint. If the API server refused the dot,
/// `kausa-` here is the whole change.
pub(crate) const NAME_PREFIX: &str = "kausa.";
/// Entries kept per object, newest first.
pub(crate) const MAX_ENTRIES: usize = 5;
/// Objects kept across the namespaces one kausa covers; the sweep deletes the oldest beyond it.
pub(crate) const MAX_OBJECTS: usize = 500;
/// The cap of an entry's `model` (the `--model` value, cleaned).
const MAX_MODEL: usize = 253;
/// The cap of an entry's `promptVersion`.
const MAX_PROMPT_VERSION: usize = 16;
/// The caps of `spec.target`'s strings.
const MAX_TARGET_API_VERSION: usize = 253;
const MAX_TARGET_KIND: usize = 63;
const MAX_TARGET_NAME: usize = 253;

/// The resource's `apiVersion` and `kind`.
const API_VERSION: &str = "kausa.fedeam.com/v1alpha1";
const KIND: &str = "Explanation";

/// The label on every object kausa creates, and its value.
pub(crate) const LABEL: &str = "app.kubernetes.io/managed-by";
pub(crate) const MANAGER: &str = "kausa";

/// The error of a `404` on the resource: the CRD is not installed.
pub(crate) const CRD_MISSING: &str = "the CustomResourceDefinition explanations.kausa.fedeam.com \
                                      is not installed (chart value history.installCRD)";
/// The error of a `409` on a create or a patch, which is not retried.
pub(crate) const CONFLICT: &str = "409 Conflict: the Explanation was created or changed by \
                                   another writer since it was read; not retried";

const CONFIDENCES: [&str; 3] = ["low", "medium", "high"];

/// The name of the Explanation of the incident with this fingerprint.
pub(crate) fn name(fingerprint: &str) -> String {
    format!("{NAME_PREFIX}{fingerprint}")
}

/// Whether `name` is the prefix and 16 lowercase hexadecimal digits.
fn is_kausa_name(name: &str) -> bool {
    name.strip_prefix(NAME_PREFIX).is_some_and(|fp| {
        fp.len() == 16 && fp.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Whether `time` is in kausa's one format, `YYYY-MM-DDTHH:MM:SSZ`.
fn is_time(time: &str) -> bool {
    let b = time.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// An Explanation's identity, its immutable `spec`: the detection id and the top-level owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Spec {
    pub(crate) rule: String,
    pub(crate) api_version: String,
    pub(crate) kind: String,
    pub(crate) name: String,
}

/// The `spec` of an incident: its detection id and its top-level owner.
pub(crate) fn spec_of(rule: &str, api_version: &str, kind: &str, name: &str) -> Spec {
    Spec {
        rule: rule.to_owned(),
        api_version: api_version.to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
    }
}

impl Spec {
    fn to_json(&self) -> Value {
        json!({
            "rule": self.rule,
            "target": {"apiVersion": self.api_version, "kind": self.kind, "name": self.name},
        })
    }

    /// A stored `spec`, if it has the schema's shape: a rule `^D[0-9]{3}$` and the three target
    /// strings within their caps.
    fn parse(spec: &Value) -> Option<Self> {
        let rule = spec.get("rule")?.as_str()?;
        let b = rule.as_bytes();
        if !(b.len() == 4 && b[0] == b'D' && b[1..].iter().all(u8::is_ascii_digit)) {
            return None;
        }
        let target = spec.get("target")?;
        let text = |field: &str, cap: usize| {
            target
                .get(field)
                .and_then(Value::as_str)
                .filter(|t| t.chars().count() <= cap)
                .map(str::to_owned)
        };
        Some(Self {
            rule: rule.to_owned(),
            api_version: text("apiVersion", MAX_TARGET_API_VERSION)?,
            kind: text("kind", MAX_TARGET_KIND)?,
            name: text("name", MAX_TARGET_NAME)?,
        })
    }
}

/// Whether an object carries kausa's label.
fn labelled(object: &Value) -> bool {
    object
        .get("metadata")
        .and_then(|m| m.get("labels"))
        .and_then(|l| l.get(LABEL))
        .and_then(Value::as_str)
        == Some(MANAGER)
}

/// Whether a listed object is one kausa may append to or delete: its label, a `kausa.` name with a
/// fingerprint, and a `spec` that parses. A convention, not proof that kausa wrote it.
pub(crate) fn is_kausa_shaped(object: &Value) -> bool {
    labelled(object)
        && object
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)
            .is_some_and(is_kausa_name)
        && object.get("spec").and_then(Spec::parse).is_some()
}

/// One distinct explanation text and when and how often kausa wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    summary: String,
    cause: String,
    steps: Vec<String>,
    confidence: String,
    model: String,
    prompt_version: String,
    first_time: String,
    last_time: String,
    count: u32,
}

impl Entry {
    /// A first write of `explanation` at `now`. The texts are cleaned again, which leaves kausa's
    /// validated text as it is (`clean` is a fixed point) and keeps any other within the schema.
    fn new(explanation: &Explanation, model: &str, now: &str) -> Self {
        Self {
            summary: prompt::clean(&explanation.summary, MAX_SUMMARY),
            cause: prompt::clean(&explanation.cause, MAX_CAUSE),
            steps: explanation
                .steps
                .iter()
                .take(MAX_STEPS)
                .map(|s| prompt::clean(s, MAX_STEP))
                .collect(),
            confidence: explanation.confidence.clone(),
            model: prompt::clean(model, MAX_MODEL),
            prompt_version: PROMPT_VERSION.to_owned(),
            first_time: now.to_owned(),
            last_time: now.to_owned(),
            count: 1,
        }
    }

    /// Whether two entries hold the same text: summary, cause, steps and confidence.
    fn same_text(&self, other: &Self) -> bool {
        self.summary == other.summary
            && self.cause == other.cause
            && self.steps == other.steps
            && self.confidence == other.confidence
    }

    /// A stored entry, if every field is there with its type, within its cap, clean, and its
    /// times in the one format. Whether a time is later than now is checked by `merge`.
    fn parse(entry: &Value) -> Option<Self> {
        let text = |field: &str, cap: usize| clean_text(entry.get(field)?, cap);
        let steps = entry.get("steps")?.as_array()?;
        if steps.len() > MAX_STEPS {
            return None;
        }
        let steps = steps
            .iter()
            .map(|s| clean_text(s, MAX_STEP))
            .collect::<Option<Vec<_>>>()?;
        let confidence = entry.get("confidence")?.as_str()?;
        if !CONFIDENCES.contains(&confidence) {
            return None;
        }
        let time = |field: &str| {
            entry
                .get(field)?
                .as_str()
                .filter(|t| is_time(t))
                .map(str::to_owned)
        };
        Some(Self {
            summary: text("summary", MAX_SUMMARY)?,
            cause: text("cause", MAX_CAUSE)?,
            steps,
            confidence: confidence.to_owned(),
            model: text("model", MAX_MODEL)?,
            prompt_version: text("promptVersion", MAX_PROMPT_VERSION)?,
            first_time: time("firstTime")?,
            last_time: time("lastTime")?,
            count: count(entry.get("count")?)?,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "summary": self.summary,
            "cause": self.cause,
            "steps": self.steps,
            "confidence": self.confidence,
            "model": self.model,
            "promptVersion": self.prompt_version,
            "firstTime": self.first_time,
            "lastTime": self.last_time,
            "count": self.count,
        })
    }
}

/// A stored text, if it is a string within `cap` characters that `clean` leaves as it is: a
/// hand-edited control, bidirectional-override or zero-width character, or markup, is never
/// written back.
fn clean_text(value: &Value, cap: usize) -> Option<String> {
    let text = value.as_str()?;
    (text.chars().count() <= cap && prompt::clean(text, cap) == text).then(|| text.to_owned())
}

/// A stored count, if it is an integer from 1 to `u32::MAX`.
fn count(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .filter(|n| *n >= 1)
        .and_then(|n| u32::try_from(n).ok())
}

/// A stored Explanation, reduced at once to what a write needs (invariant 8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stored {
    pub(crate) labelled: bool,
    pub(crate) spec: Option<Spec>,
    pub(crate) resource_version: Option<String>,
    /// The entries that parse, in their stored order; `merge` drops those dated after now, the
    /// repeated texts and those beyond `MAX_ENTRIES`.
    pub(crate) entries: Vec<Entry>,
    /// `status.occurrences`, if it is an integer from 1 to `u32::MAX`.
    pub(crate) occurrences: Option<u32>,
}

/// Reduces an object, as a list returns it, to its label, `spec`, `resourceVersion`, the entries
/// that parse and `occurrences`. Any JSON value is accepted.
pub(crate) fn parse_stored(object: &Value) -> Stored {
    let status = object.get("status");
    Stored {
        labelled: labelled(object),
        spec: object.get("spec").and_then(Spec::parse),
        resource_version: object
            .get("metadata")
            .and_then(|m| m.get("resourceVersion"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        entries: status
            .and_then(|s| s.get("entries"))
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Entry::parse).collect())
            .unwrap_or_default(),
        occurrences: status.and_then(|s| s.get("occurrences")).and_then(count),
    }
}

/// Whether kausa may write to a stored object for this incident: it carries the label and the
/// incident's `spec`. The reason of a refusal names the object by the incident's fingerprint and
/// never quotes what was stored.
pub(crate) fn check_identity(
    stored: &Stored,
    fingerprint: &str,
    spec: &Spec,
) -> Result<(), String> {
    let name = name(fingerprint);
    if !stored.labelled {
        return Err(format!(
            "the Explanation {name} does not carry the label {LABEL}={MANAGER}"
        ));
    }
    if stored.spec.as_ref() != Some(spec) {
        return Err(format!(
            "the Explanation {name} is about another rule or target"
        ));
    }
    Ok(())
}

/// The `status` kausa writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    occurrences: u32,
    last_time: String,
    entries: Vec<Entry>,
}

impl Status {
    fn to_json(&self) -> Value {
        json!({
            "occurrences": self.occurrences,
            "lastTime": self.last_time,
            "entries": self.entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
        })
    }
}

/// The body that creates the Explanation of an incident: its name and label as the only
/// `metadata` (a namespaced Api sets the namespace; no owner references), its `spec`, and a
/// `status` of one entry.
pub(crate) fn create_body(
    fingerprint: &str,
    spec: &Spec,
    explanation: &Explanation,
    model: &str,
    now: &str,
) -> Value {
    let status = Status {
        occurrences: 1,
        last_time: now.to_owned(),
        entries: vec![Entry::new(explanation, model, now)],
    };
    json!({
        "apiVersion": API_VERSION,
        "kind": KIND,
        "metadata": {"name": name(fingerprint), "labels": {LABEL: MANAGER}},
        "spec": spec.to_json(),
        "status": status.to_json(),
    })
}

/// The status after writing `explanation` again into a stored object, at `now`, keeping entries
/// written at or after `cutoff`.
///
/// Stored entries dated after `now` are dropped (they would stay out of the age cut for good), a
/// repeated text keeps its first entry, and at most `MAX_ENTRIES` are read. The same text counts
/// and moves to the front with the current model and prompt version; a new text goes in front.
/// Then the entries older than `cutoff` go, and the list is cut to `MAX_ENTRIES`. An unreadable
/// `occurrences` counts as the sum of the entries read. Counts saturate.
pub(crate) fn merge(
    stored: &Stored,
    explanation: &Explanation,
    model: &str,
    now: &str,
    cutoff: &str,
) -> Status {
    let mut entries: Vec<Entry> = Vec::with_capacity(MAX_ENTRIES + 1);
    for entry in &stored.entries {
        if entries.len() == MAX_ENTRIES {
            break;
        }
        if entry.first_time.as_str() > now
            || entry.last_time.as_str() > now
            || entries.iter().any(|e| e.same_text(entry))
        {
            continue;
        }
        entries.push(entry.clone());
    }
    let occurrences = stored.occurrences.unwrap_or_else(|| {
        entries
            .iter()
            .fold(0u32, |sum, e| sum.saturating_add(e.count))
    });
    let fresh = Entry::new(explanation, model, now);
    let entry = match entries.iter().position(|e| e.same_text(&fresh)) {
        Some(i) => {
            let old = entries.remove(i);
            Entry {
                first_time: old.first_time,
                count: old.count.saturating_add(1),
                ..fresh
            }
        }
        None => fresh,
    };
    entries.insert(0, entry);
    entries.retain(|e| e.last_time.as_str() >= cutoff);
    entries.truncate(MAX_ENTRIES);
    Status {
        occurrences: occurrences.saturating_add(1),
        last_time: now.to_owned(),
        entries,
    }
}

/// The JSON merge patch of a write: the `resourceVersion` read, as a precondition, and the
/// `status`. No other `metadata` key is ever sent.
pub(crate) fn patch_body(resource_version: &str, status: &Status) -> Value {
    json!({
        "metadata": {"resourceVersion": resource_version},
        "status": status.to_json(),
    })
}

/// The outcome of a failed create or patch, by HTTP status: a `409` is a conflict (not retried),
/// anything else an error.
pub(crate) fn failure_outcome(code: Option<u16>) -> &'static str {
    match code {
        Some(409) => "conflict",
        _ => "error",
    }
}

/// Why the sweep deletes an object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    /// Its time is before the cutoff of `--history-days`.
    Age,
    /// `MAX_OBJECTS` newer objects were seen.
    Cap,
}

impl Reason {
    /// The value of the `reason` label of `kausa_history_deleted_total`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Age => "age",
            Self::Cap => "cap",
        }
    }
}

/// One delete of the sweep, sent with `resource_version` as a precondition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Delete {
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) resource_version: String,
    pub(crate) reason: Reason,
}

/// A listed object reduced to what the sweep needs, ordered by time, then namespace, then name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Held {
    time: String,
    namespace: String,
    name: String,
    resource_version: String,
}

impl Held {
    fn delete(self, reason: Reason) -> Delete {
        Delete {
            namespace: self.namespace,
            name: self.name,
            resource_version: self.resource_version,
            reason,
        }
    }
}

/// The sweep's plan, fed one page of the list at a time. Objects older than the cutoff are deleted
/// as their page arrives; the others are held in a min-heap of at most `MAX_OBJECTS`, and the
/// oldest one pushed past it is deleted at once: `MAX_OBJECTS` newer objects were seen, so it is
/// beyond the cap however the list ends. Nothing still held is ever deleted.
#[derive(Debug)]
pub(crate) struct SweepPlan {
    now: String,
    cutoff: String,
    heap: BinaryHeap<Reverse<Held>>,
}

impl SweepPlan {
    pub(crate) fn new(now: &str, cutoff: &str) -> Self {
        Self {
            now: now.to_owned(),
            cutoff: cutoff.to_owned(),
            heap: BinaryHeap::with_capacity(MAX_OBJECTS + 1),
        }
    }

    /// The deletes one page of listed objects calls for. Objects that are not kausa-shaped, or
    /// have no readable time, namespace or `resourceVersion`, are neither counted nor deleted.
    pub(crate) fn page(&mut self, items: &[Value]) -> Vec<Delete> {
        let mut deletes = Vec::new();
        for item in items {
            let Some(held) = self.reduce(item) else {
                continue;
            };
            if held.time < self.cutoff {
                deletes.push(held.delete(Reason::Age));
                continue;
            }
            self.heap.push(Reverse(held));
            if self.heap.len() > MAX_OBJECTS
                && let Some(Reverse(oldest)) = self.heap.pop()
            {
                deletes.push(oldest.delete(Reason::Cap));
            }
        }
        deletes
    }

    /// How many objects the plan holds for the cap (for the tests and the fuzz target).
    #[allow(dead_code)]
    pub(crate) fn held(&self) -> usize {
        self.heap.len()
    }

    /// A listed object as a tuple: its time is `status.lastTime` if it is in the one format and
    /// not after now, else `metadata.creationTimestamp` if that is in the one format.
    fn reduce(&self, item: &Value) -> Option<Held> {
        if !is_kausa_shaped(item) {
            return None;
        }
        let metadata = item.get("metadata")?;
        let string = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
        let last = item
            .get("status")
            .and_then(|s| s.get("lastTime"))
            .and_then(Value::as_str)
            .filter(|t| is_time(t) && *t <= self.now.as_str());
        let time = match last {
            Some(t) => t.to_owned(),
            None => metadata
                .get("creationTimestamp")
                .and_then(Value::as_str)
                .filter(|t| is_time(t))?
                .to_owned(),
        };
        Some(Held {
            time,
            namespace: string(metadata.get("namespace"))?,
            name: string(metadata.get("name"))?,
            resource_version: string(metadata.get("resourceVersion"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::{
        self, Explanation, MAX_CAUSE, MAX_STEP, MAX_STEPS, MAX_SUMMARY, PROMPT_VERSION,
    };
    use serde_json::{Map, Value, json};
    use std::collections::BTreeSet;

    // Invented values only: no real cluster output (AGENTS.md, Security rules).
    const FP: &str = "0123456789abcdef";
    const NOW: &str = "2026-10-28T10:00:00Z";
    const CUTOFF: &str = "2026-10-21T10:00:00Z";
    const CAUSE: &str = "The image tag does not exist in registry.example.com.";
    const STEP: &str = "Check the image tag of the Deployment.";
    const LABEL: &str = "app.kubernetes.io/managed-by";

    fn spec() -> Spec {
        Spec {
            rule: "D001".into(),
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "api".into(),
        }
    }

    fn explanation(summary: &str) -> Explanation {
        Explanation {
            summary: summary.into(),
            cause: CAUSE.into(),
            steps: vec![STEP.into()],
            confidence: "high".into(),
        }
    }

    /// A stored entry with the text of `explanation(summary)`.
    fn entry(summary: &str, first: &str, last: &str, count: u64) -> Value {
        json!({
            "summary": summary,
            "cause": CAUSE,
            "steps": [STEP],
            "confidence": "high",
            "model": "old-model",
            "promptVersion": "0",
            "firstTime": first,
            "lastTime": last,
            "count": count,
        })
    }

    /// A stored Explanation, as a list by name returns it.
    fn object(entries: Value, occurrences: Value) -> Value {
        json!({
            "apiVersion": "kausa.fedeam.com/v1alpha1",
            "kind": "Explanation",
            "metadata": {
                "name": name(FP),
                "namespace": "shop",
                "resourceVersion": "41",
                "creationTimestamp": "2026-10-20T10:00:00Z",
                "labels": {LABEL: "kausa"},
            },
            "spec": {
                "rule": "D001",
                "target": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "api"},
            },
            "status": {
                "occurrences": occurrences,
                "lastTime": "2026-10-27T10:00:00Z",
                "entries": entries,
            },
        })
    }

    /// The status a merge of `explanation` into `stored` would patch.
    fn merged(stored: &Value, explanation: &Explanation, model: &str) -> Value {
        let parsed = parse_stored(stored);
        let status = merge(&parsed, explanation, model, NOW, CUTOFF);
        patch_body("41", &status)["status"].clone()
    }

    fn entries(status: &Value) -> Vec<Value> {
        status["entries"].as_array().cloned().unwrap_or_default()
    }

    fn summaries(status: &Value) -> Vec<String> {
        entries(status)
            .iter()
            .map(|e| e["summary"].as_str().unwrap_or("<not a string>").to_owned())
            .collect()
    }

    fn remove(value: &mut Value, key: &str) {
        if let Some(object) = value.as_object_mut() {
            object.remove(key);
        }
    }

    fn keys(value: &Value) -> BTreeSet<String> {
        value
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    const ENTRY_FIELDS: [&str; 9] = [
        "summary",
        "cause",
        "steps",
        "confidence",
        "model",
        "promptVersion",
        "firstTime",
        "lastTime",
        "count",
    ];

    /// Whether `time` is in kausa's one format, `YYYY-MM-DDTHH:MM:SSZ`.
    fn is_time(time: &str) -> bool {
        let b = time.as_bytes();
        b.len() == 20
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                19 => *c == b'Z',
                _ => c.is_ascii_digit(),
            })
    }

    /// What every status kausa builds holds, whatever it read: the caps, clean texts, known
    /// confidences, times in the one format and not after `NOW`, counts of at least 1.
    fn assert_well_formed(status: &Value, case: &str) {
        assert_eq!(
            keys(status),
            set(&["occurrences", "lastTime", "entries"]),
            "{case}"
        );
        assert!(
            status["occurrences"]
                .as_u64()
                .is_some_and(|n| n >= 1 && n <= u64::from(u32::MAX)),
            "{case}"
        );
        assert_eq!(status["lastTime"], NOW, "{case}");
        let list = entries(status);
        assert!(!list.is_empty() && list.len() <= MAX_ENTRIES, "{case}");
        for e in &list {
            assert_eq!(keys(e), set(&ENTRY_FIELDS), "{case}: {e}");
            let text = |field: &str, cap: usize| {
                let t = e[field]
                    .as_str()
                    .unwrap_or_else(|| panic!("{case}: {field}"));
                assert!(t.chars().count() <= cap, "{case}: {field}");
                assert_eq!(prompt::clean(t, cap), t, "{case}: {field} is not clean");
            };
            text("summary", MAX_SUMMARY);
            text("cause", MAX_CAUSE);
            text("model", 253);
            text("promptVersion", 16);
            let steps = e["steps"]
                .as_array()
                .unwrap_or_else(|| panic!("{case}: steps"));
            assert!(steps.len() <= MAX_STEPS, "{case}");
            for step in steps {
                let t = step.as_str().unwrap_or_else(|| panic!("{case}: a step"));
                assert!(t.chars().count() <= MAX_STEP, "{case}");
                assert_eq!(prompt::clean(t, MAX_STEP), t, "{case}: a step is not clean");
            }
            assert!(
                matches!(e["confidence"].as_str(), Some("low" | "medium" | "high")),
                "{case}"
            );
            for field in ["firstTime", "lastTime"] {
                let t = e[field]
                    .as_str()
                    .unwrap_or_else(|| panic!("{case}: {field}"));
                assert!(is_time(t) && t <= NOW, "{case}: {field} {t}");
            }
            assert!(
                e["count"]
                    .as_u64()
                    .is_some_and(|n| n >= 1 && n <= u64::from(u32::MAX)),
                "{case}"
            );
        }
    }

    // --- Names and bodies ---------------------------------------------------------------------

    #[test]
    fn the_name_is_the_prefix_and_the_fingerprint() {
        assert_eq!(NAME_PREFIX, "kausa.");
        assert_eq!(name(FP), "kausa.0123456789abcdef");
        assert_eq!(name(FP), format!("{NAME_PREFIX}{FP}"));
    }

    #[test]
    fn the_create_body_is_exactly_the_contract() {
        let body = create_body(
            FP,
            &spec(),
            &explanation("pod-1 crashes on start"),
            "gpt-x",
            NOW,
        );
        assert_eq!(
            body,
            json!({
                "apiVersion": "kausa.fedeam.com/v1alpha1",
                "kind": "Explanation",
                "metadata": {
                    "name": "kausa.0123456789abcdef",
                    "labels": {"app.kubernetes.io/managed-by": "kausa"},
                },
                "spec": {
                    "rule": "D001",
                    "target": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "api"},
                },
                "status": {
                    "occurrences": 1,
                    "lastTime": NOW,
                    "entries": [{
                        "summary": "pod-1 crashes on start",
                        "cause": CAUSE,
                        "steps": [STEP],
                        "confidence": "high",
                        "model": "gpt-x",
                        "promptVersion": PROMPT_VERSION,
                        "firstTime": NOW,
                        "lastTime": NOW,
                        "count": 1,
                    }],
                },
            })
        );
        // Only the name and the label: no namespace (a namespaced Api sets it), no owner
        // references, no annotations or finalizers.
        assert_eq!(keys(&body["metadata"]), set(&["name", "labels"]));
        assert_eq!(keys(&body["metadata"]["labels"]), set(&[LABEL]));
    }

    #[test]
    fn steps_may_be_empty() {
        let mut e = explanation("no steps");
        e.steps.clear();
        let body = create_body(FP, &spec(), &e, "m", NOW);
        assert_eq!(body["status"]["entries"][0]["steps"], json!([]));
    }

    #[test]
    fn the_model_is_cleaned_and_cut_to_253_characters_on_a_character_boundary() {
        let long = "é".repeat(300);
        let body = create_body(FP, &spec(), &explanation("s"), &long, NOW);
        assert_eq!(body["status"]["entries"][0]["model"], "é".repeat(253));
        let body = create_body(
            FP,
            &spec(),
            &explanation("s"),
            "m\u{0007}x <b>\u{202E}",
            NOW,
        );
        assert_eq!(body["status"]["entries"][0]["model"], "m x b");
        // The same in a merge.
        let stored = object(json!([]), json!(3));
        let status = merged(&stored, &explanation("s"), &long);
        assert_eq!(status["entries"][0]["model"], "é".repeat(253));
    }

    #[test]
    fn the_patch_body_holds_only_the_resource_version_and_the_status() {
        let stored = object(
            json!([entry(
                "a",
                "2026-10-25T10:00:00Z",
                "2026-10-27T10:00:00Z",
                2
            )]),
            json!(2),
        );
        let status = merge(&parse_stored(&stored), &explanation("b"), "m", NOW, CUTOFF);
        let body = patch_body("41", &status);
        assert_eq!(keys(&body), set(&["metadata", "status"]));
        assert_eq!(body["metadata"], json!({"resourceVersion": "41"}));
        assert_eq!(
            keys(&body["status"]),
            set(&["occurrences", "lastTime", "entries"])
        );
        for e in entries(&body["status"]) {
            assert_eq!(keys(&e), set(&ENTRY_FIELDS));
        }
    }

    // --- The merge ----------------------------------------------------------------------------

    #[test]
    fn the_same_text_counts_and_moves_to_the_front() {
        let stored = object(
            json!([
                entry("a", "2026-10-25T10:00:00Z", "2026-10-27T10:00:00Z", 1),
                entry("b", "2026-10-22T10:00:00Z", "2026-10-26T10:00:00Z", 3),
            ]),
            json!(4),
        );
        let status = merged(&stored, &explanation("b"), "new-model");
        assert_well_formed(&status, "same text");
        assert_eq!(status["occurrences"], 5);
        assert_eq!(status["lastTime"], NOW);
        assert_eq!(
            entries(&status),
            vec![
                json!({
                    "summary": "b",
                    "cause": CAUSE,
                    "steps": [STEP],
                    "confidence": "high",
                    "model": "new-model",
                    "promptVersion": PROMPT_VERSION,
                    "firstTime": "2026-10-22T10:00:00Z",
                    "lastTime": NOW,
                    "count": 4,
                }),
                entry("a", "2026-10-25T10:00:00Z", "2026-10-27T10:00:00Z", 1),
            ]
        );
    }

    #[test]
    fn a_new_text_goes_first_with_a_count_of_one() {
        let a = entry("a", "2026-10-25T10:00:00Z", "2026-10-27T10:00:00Z", 1);
        let b = entry("b", "2026-10-22T10:00:00Z", "2026-10-26T10:00:00Z", 3);
        let stored = object(json!([a, b]), json!(4));
        let status = merged(&stored, &explanation("c"), "m");
        assert_well_formed(&status, "new text");
        assert_eq!(status["occurrences"], 5);
        let list = entries(&status);
        assert_eq!(list.len(), 3);
        assert_eq!(
            list[0],
            json!({
                "summary": "c",
                "cause": CAUSE,
                "steps": [STEP],
                "confidence": "high",
                "model": "m",
                "promptVersion": PROMPT_VERSION,
                "firstTime": NOW,
                "lastTime": NOW,
                "count": 1,
            })
        );
        assert_eq!(list[1..], [a, b]);
    }

    #[test]
    fn a_text_is_the_summary_the_cause_the_steps_and_the_confidence() {
        let stored = || {
            object(
                json!([entry(
                    "a",
                    "2026-10-25T10:00:00Z",
                    "2026-10-27T10:00:00Z",
                    2
                )]),
                json!(2),
            )
        };
        let mut other_confidence = explanation("a");
        other_confidence.confidence = "low".into();
        let mut other_cause = explanation("a");
        other_cause.cause = "Another cause.".into();
        let mut other_steps = explanation("a");
        other_steps.steps.push("One more step.".into());
        let mut no_steps = explanation("a");
        no_steps.steps.clear();
        for (case, e) in [
            ("confidence", other_confidence),
            ("cause", other_cause),
            ("one more step", other_steps),
            ("no steps", no_steps),
        ] {
            let status = merged(&stored(), &e, "m");
            let list = entries(&status);
            assert_eq!(list.len(), 2, "{case}");
            assert_eq!(list[0]["count"], 1, "{case}");
            assert_eq!(list[1]["count"], 2, "{case}");
        }
    }

    #[test]
    fn the_sixth_text_drops_the_stalest() {
        let stored = object(
            json!([
                entry("s1", "2026-10-27T05:00:00Z", "2026-10-27T05:00:00Z", 1),
                entry("s2", "2026-10-27T04:00:00Z", "2026-10-27T04:00:00Z", 1),
                entry("s3", "2026-10-27T03:00:00Z", "2026-10-27T03:00:00Z", 1),
                entry("s4", "2026-10-27T02:00:00Z", "2026-10-27T02:00:00Z", 1),
                entry("s5", "2026-10-27T01:00:00Z", "2026-10-27T01:00:00Z", 1),
            ]),
            json!(5),
        );
        let status = merged(&stored, &explanation("new"), "m");
        assert_well_formed(&status, "sixth");
        assert_eq!(summaries(&status), ["new", "s1", "s2", "s3", "s4"]);
        assert_eq!(status["occurrences"], 6);
    }

    #[test]
    fn entries_older_than_the_cutoff_are_dropped() {
        let stored = object(
            json!([
                entry("fresh", "2026-10-27T10:00:00Z", "2026-10-27T10:00:00Z", 1),
                entry("at the cutoff", "2026-10-20T10:00:00Z", CUTOFF, 1),
                entry(
                    "a second older",
                    "2026-10-20T10:00:00Z",
                    "2026-10-21T09:59:59Z",
                    1
                ),
                entry(
                    "much older",
                    "2026-01-01T00:00:00Z",
                    "2026-01-02T00:00:00Z",
                    7
                ),
            ]),
            json!(10),
        );
        let status = merged(&stored, &explanation("new"), "m");
        assert_well_formed(&status, "age");
        assert_eq!(summaries(&status), ["new", "fresh", "at the cutoff"]);
        assert_eq!(status["occurrences"], 11);
    }

    #[test]
    fn occurrences_keep_counting_after_entries_drop() {
        let stored = object(
            json!([entry(
                "old",
                "2026-01-01T00:00:00Z",
                "2026-01-02T00:00:00Z",
                39
            )]),
            json!(40),
        );
        let status = merged(&stored, &explanation("new"), "m");
        assert_eq!(summaries(&status), ["new"]);
        assert_eq!(status["occurrences"], 41);
    }

    #[test]
    fn counts_saturate() {
        let max = u64::from(u32::MAX);
        let stored = object(
            json!([entry(
                "a",
                "2026-10-25T10:00:00Z",
                "2026-10-27T10:00:00Z",
                max
            )]),
            json!(max),
        );
        let status = merged(&stored, &explanation("a"), "m");
        assert_eq!(status["occurrences"], max);
        assert_eq!(status["entries"][0]["count"], max);
    }

    #[test]
    fn unreadable_occurrences_become_the_sum_of_the_kept_counts() {
        let list = json!([
            entry("a", "2026-10-25T10:00:00Z", "2026-10-27T10:00:00Z", 2),
            entry("b", "2026-10-25T10:00:00Z", "2026-10-26T10:00:00Z", 3),
            entry("old", "2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z", 50),
        ]);
        for occurrences in [
            json!(0),
            json!(-1),
            json!("7"),
            json!(1.5),
            json!(null),
            json!(u64::from(u32::MAX) + 1),
            json!([7]),
        ] {
            let status = merged(
                &object(list.clone(), occurrences.clone()),
                &explanation("c"),
                "m",
            );
            // The entries kept when read (2 + 3 + 50: the old one is valid input, and the age cut
            // comes after), plus this write.
            assert_eq!(status["occurrences"], 56, "{occurrences}");
        }
        // Missing altogether.
        let mut stored = object(
            json!([entry("a", "2026-10-25T10:00:00Z", NOW, 2)]),
            json!(1),
        );
        remove(&mut stored["status"], "occurrences");
        assert_eq!(merged(&stored, &explanation("c"), "m")["occurrences"], 3);
        // A sum past u32::MAX saturates instead of panicking.
        let max = u64::from(u32::MAX);
        let stored = object(
            json!([
                entry("a", "2026-10-25T10:00:00Z", NOW, max),
                entry("b", "2026-10-25T10:00:00Z", NOW, max),
            ]),
            json!(null),
        );
        assert_eq!(merged(&stored, &explanation("c"), "m")["occurrences"], max);
    }

    #[test]
    fn kausa_s_own_entries_survive_a_re_read() {
        // Texts as the model's answer gives them, cut at their caps: the cut falls on a space and
        // inside a marker.
        let answer = json!({
            "summary": format!("{} tail", "x".repeat(MAX_SUMMARY - 1)),
            "cause": format!("{} <redacted:ip> and more", "y".repeat(MAX_CAUSE - 8)),
            "steps": [format!("{} <redacted:hostname>", "z".repeat(MAX_STEP - 5)), "Check it."],
            "confidence": "medium",
        });
        let e = prompt::validate(&answer.to_string()).expect("a valid answer");
        let model = format!("{} x", "m".repeat(252));
        let mut stored = create_body(FP, &spec(), &e, &model, "2026-10-27T10:00:00Z");
        stored["metadata"]["namespace"] = json!("shop");
        stored["metadata"]["resourceVersion"] = json!("7");
        let status = merged(&stored, &e, &model);
        assert_well_formed(&status, "re-read");
        assert_eq!(status["occurrences"], 2);
        let list = entries(&status);
        assert_eq!(list.len(), 1, "kausa dropped its own entry: {status}");
        assert_eq!(list[0]["count"], 2);
        assert_eq!(list[0]["firstTime"], "2026-10-27T10:00:00Z");
    }

    // --- Stored entries are untrusted ---------------------------------------------------------

    /// The status after merging the text "fresh" into an object whose only entry is `bad`.
    fn merged_with(bad: Value) -> Value {
        merged(&object(json!([bad]), json!(9)), &explanation("fresh"), "m")
    }

    #[test]
    fn a_valid_stored_entry_is_kept() {
        let status = merged_with(entry(
            "stored",
            "2026-10-25T10:00:00Z",
            "2026-10-27T10:00:00Z",
            2,
        ));
        assert_eq!(summaries(&status), ["fresh", "stored"]);
        assert_eq!(status["occurrences"], 10);
    }

    #[test]
    fn malformed_entries_are_dropped() {
        let valid = entry("stored", "2026-10-25T10:00:00Z", "2026-10-27T10:00:00Z", 2);
        let mut cases: Vec<(String, Value)> = Vec::new();
        for field in ENTRY_FIELDS {
            let mut e = valid.clone();
            remove(&mut e, field);
            cases.push((format!("no {field}"), e));
        }
        let with = |field: &str, value: Value| {
            let mut e = valid.clone();
            e[field] = value;
            (format!("{field} = {}", e[field]), e)
        };
        cases.extend([
            with("summary", json!(5)),
            with("summary", json!(null)),
            with("summary", json!(["stored"])),
            with("cause", json!({"text": "c"})),
            with("steps", json!(STEP)),
            with("steps", json!([1])),
            with("steps", json!([STEP, null])),
            with("confidence", json!("HIGH")),
            with("confidence", json!("sure")),
            with("confidence", json!(3)),
            with("model", json!(1)),
            with("promptVersion", json!(1)),
            with("count", json!("2")),
            with("count", json!(0)),
            with("count", json!(-1)),
            with("count", json!(1.5)),
            with("count", json!(u64::from(u32::MAX) + 1)),
            with("count", json!(null)),
        ]);
        for time in [
            json!("2026-10-27T10:00:00.000Z"),
            json!("2026-10-27T10:00:00+00:00"),
            json!("2026-10-27 10:00:00Z"),
            json!("2026-10-27T10:00:00z"),
            json!("2026-10-27T10:00Z"),
            json!("2026-10-27"),
            json!(""),
            json!("yesterday"),
            json!(1_761_559_200),
            json!(null),
        ] {
            cases.push(with("firstTime", time.clone()));
            cases.push(with("lastTime", time));
        }
        // Later than now: it would stay out of the age cut for good.
        for time in ["2026-10-28T10:00:01Z", "2099-01-01T00:00:00Z"] {
            cases.push(with("firstTime", json!(time)));
            cases.push(with("lastTime", json!(time)));
        }
        // Above the caps, in characters.
        cases.extend([
            with("summary", json!("é".repeat(MAX_SUMMARY + 1))),
            with("cause", json!("é".repeat(MAX_CAUSE + 1))),
            with("steps", json!(["é".repeat(MAX_STEP + 1)])),
            with("steps", json!(vec!["s"; MAX_STEPS + 1])),
            with("model", json!("é".repeat(254))),
            with("promptVersion", json!("1".repeat(17))),
        ]);
        for (case, bad) in cases {
            let status = merged_with(bad);
            assert_well_formed(&status, &case);
            assert_eq!(summaries(&status), ["fresh"], "{case}");
            assert_eq!(status["occurrences"], 10, "{case}");
        }
    }

    #[test]
    fn texts_at_the_caps_are_kept() {
        let mut e = entry(
            &"é".repeat(MAX_SUMMARY),
            "2026-10-25T10:00:00Z",
            "2026-10-27T10:00:00Z",
            2,
        );
        e["cause"] = json!("é".repeat(MAX_CAUSE));
        e["steps"] = json!(vec!["é".repeat(MAX_STEP); MAX_STEPS]);
        e["model"] = json!("é".repeat(253));
        e["promptVersion"] = json!("1".repeat(16));
        let status = merged_with(e);
        assert_well_formed(&status, "at the caps");
        assert_eq!(entries(&status).len(), 2);
    }

    #[test]
    fn unclean_entries_are_dropped() {
        let valid = entry("stored", "2026-10-25T10:00:00Z", "2026-10-27T10:00:00Z", 2);
        let hostile = [
            "ok\u{202E}evil",
            "zero\u{200B}width",
            "bell\u{0007}ring",
            "new\nline",
            "<b>bold</b>",
            "`code`",
            "a <redacted:ip",
            "two  spaces",
            " leading",
            "trailing ",
            // Not NFC: clean composes it.
            "cafe\u{0301}",
        ];
        for text in hostile {
            for field in ["summary", "cause", "model", "promptVersion"] {
                let mut e = valid.clone();
                e[field] = json!(text);
                let status = merged_with(e);
                assert_well_formed(&status, &format!("{field} {text:?}"));
                assert_eq!(summaries(&status), ["fresh"], "{field} {text:?}");
            }
            let mut e = valid.clone();
            e["steps"] = json!([STEP, text]);
            let status = merged_with(e);
            assert_eq!(summaries(&status), ["fresh"], "a step {text:?}");
        }
    }

    #[test]
    fn duplicate_texts_keep_the_first() {
        let stored = object(
            json!([
                entry("a", "2026-10-25T10:00:00Z", "2026-10-27T10:00:00Z", 2),
                entry("a", "2026-10-22T10:00:00Z", "2026-10-26T10:00:00Z", 9),
            ]),
            json!(11),
        );
        let status = merged(&stored, &explanation("c"), "m");
        assert_eq!(summaries(&status), ["c", "a"]);
        assert_eq!(status["entries"][1]["count"], 2);
        assert_eq!(status["entries"][1]["firstTime"], "2026-10-25T10:00:00Z");
    }

    #[test]
    fn at_most_five_valid_entries_are_read() {
        let mut list: Vec<Value> = vec![json!("junk"), json!({"summary": "x"})];
        list.extend((1..=7).map(|i| {
            let t = format!("2026-10-27T0{i}:00:00Z");
            entry(&format!("s{i}"), &t, &t, 1)
        }));
        // s6 is beyond the five entries read, so it comes back as a new text.
        let status = merged(&object(json!(list), json!(7)), &explanation("s6"), "m");
        assert_well_formed(&status, "seven stored");
        assert_eq!(summaries(&status), ["s6", "s1", "s2", "s3", "s4"]);
        assert_eq!(status["entries"][0]["count"], 1);
        assert_eq!(status["entries"][0]["firstTime"], NOW);
    }

    #[test]
    fn malformed_statuses_never_panic_and_are_cut() {
        let many: Vec<Value> = (0..1000)
            .map(|i| {
                entry(
                    &format!("s{i}"),
                    "2026-10-27T10:00:00Z",
                    "2026-10-27T10:00:00Z",
                    1,
                )
            })
            .collect();
        let deep = (0..200).fold(json!("x"), |inner, _| json!([inner]));
        let objects = [
            json!(null),
            json!(true),
            json!(5),
            json!("kausa"),
            json!([]),
            json!({}),
            json!({"metadata": 5, "spec": [], "status": "x"}),
            json!({"metadata": {"labels": [LABEL]}, "status": {}}),
            object(json!("entries"), json!(1)),
            object(json!({"0": entry("a", NOW, NOW, 1)}), json!(1)),
            object(json!(null), json!(1)),
            object(json!(many), json!(1000)),
            object(json!([deep, 1, null, "x", [], {}]), json!(1)),
            object(json!([entry("a", NOW, NOW, 1)]), json!({"n": 1})),
        ];
        for (i, stored) in objects.iter().enumerate() {
            let parsed = parse_stored(stored);
            let status = merge(&parsed, &explanation("fresh"), "m", NOW, CUTOFF);
            let body = patch_body("1", &status);
            assert_well_formed(&body["status"], &format!("object {i}"));
            assert_eq!(
                body["status"]["entries"][0]["summary"], "fresh",
                "object {i}"
            );
        }
        let status = merged(&objects[11], &explanation("fresh"), "m");
        assert_eq!(summaries(&status), ["fresh", "s0", "s1", "s2", "s3"]);
    }

    // --- Reading a stored object and the identity check ---------------------------------------

    #[test]
    fn a_stored_object_is_reduced_to_its_label_spec_and_resource_version() {
        let stored = parse_stored(&object(json!([]), json!(1)));
        assert!(stored.labelled);
        assert_eq!(stored.spec, Some(spec()));
        assert_eq!(stored.resource_version.as_deref(), Some("41"));
        for labels in [
            json!(null),
            json!({}),
            json!({LABEL: "Kausa"}),
            json!({LABEL: "helm"}),
            json!([LABEL]),
        ] {
            let mut o = object(json!([]), json!(1));
            o["metadata"]["labels"] = labels.clone();
            assert!(!parse_stored(&o).labelled, "{labels}");
        }
        let mut o = object(json!([]), json!(1));
        remove(&mut o["metadata"], "resourceVersion");
        assert_eq!(parse_stored(&o).resource_version, None);
    }

    /// Specs that do not parse: the rule's pattern, the three target strings and their caps.
    fn bad_specs() -> Vec<Value> {
        let target = json!({"apiVersion": "apps/v1", "kind": "Deployment", "name": "api"});
        let with_target = |field: &str, value: Value| {
            let mut t = target.clone();
            if value.is_null() {
                remove(&mut t, field);
            } else {
                t[field] = value;
            }
            json!({"rule": "D001", "target": t})
        };
        let mut specs = vec![
            json!(null),
            json!("D001"),
            json!({}),
            json!({"rule": "D001"}),
            json!({"target": target}),
            json!({"rule": 1, "target": target}),
            json!({"rule": "D01", "target": target}),
            json!({"rule": "D0001", "target": target}),
            json!({"rule": "X001", "target": target}),
            json!({"rule": "d001", "target": target}),
            json!({"rule": " D001", "target": target}),
            json!({"rule": "D001", "target": "api"}),
        ];
        for field in ["apiVersion", "kind", "name"] {
            specs.push(with_target(field, json!(null)));
            specs.push(with_target(field, json!(5)));
        }
        specs.push(with_target("apiVersion", json!("a".repeat(254))));
        specs.push(with_target("kind", json!("a".repeat(64))));
        specs.push(with_target("name", json!("a".repeat(254))));
        specs
    }

    #[test]
    fn a_spec_that_does_not_parse_is_none() {
        for bad in bad_specs() {
            let mut o = object(json!([]), json!(1));
            o["spec"] = bad.clone();
            assert_eq!(parse_stored(&o).spec, None, "{bad}");
        }
        // At the caps it parses.
        let mut o = object(json!([]), json!(1));
        o["spec"]["target"] = json!({
            "apiVersion": "a".repeat(253), "kind": "a".repeat(63), "name": "a".repeat(253),
        });
        assert!(parse_stored(&o).spec.is_some());
    }

    fn label_refusal() -> String {
        format!(
            "the Explanation {} does not carry the label app.kubernetes.io/managed-by=kausa",
            name(FP)
        )
    }

    fn target_refusal() -> String {
        format!(
            "the Explanation {} is about another rule or target",
            name(FP)
        )
    }

    #[test]
    fn the_identity_check_refuses_a_missing_label_another_rule_or_another_target() {
        let stored = parse_stored(&object(json!([]), json!(1)));
        assert_eq!(check_identity(&stored, FP, &spec()), Ok(()));

        let mut unlabelled = object(json!([]), json!(1));
        remove(&mut unlabelled["metadata"], "labels");
        assert_eq!(
            check_identity(&parse_stored(&unlabelled), FP, &spec()),
            Err(label_refusal())
        );
        // The label is checked first.
        unlabelled["spec"]["rule"] = json!("D002");
        assert_eq!(
            check_identity(&parse_stored(&unlabelled), FP, &spec()),
            Err(label_refusal())
        );

        for (field, value) in [
            ("rule", "D002"),
            ("apiVersion", "apps/v2"),
            ("kind", "StatefulSet"),
            ("name", "web"),
        ] {
            let mut o = object(json!([]), json!(1));
            if field == "rule" {
                o["spec"]["rule"] = json!(value);
            } else {
                o["spec"]["target"][field] = json!(value);
            }
            assert_eq!(
                check_identity(&parse_stored(&o), FP, &spec()),
                Err(target_refusal()),
                "{field}"
            );
        }
        for bad in bad_specs() {
            let mut o = object(json!([]), json!(1));
            o["spec"] = bad.clone();
            assert_eq!(
                check_identity(&parse_stored(&o), FP, &spec()),
                Err(target_refusal()),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_refusal_never_quotes_the_stored_object() {
        let mut o = object(json!([]), json!(1));
        o["spec"]["target"]["name"] = json!("evil\u{202E}name");
        o["metadata"]["labels"] = json!({LABEL: "forged\nline"});
        let reason = check_identity(&parse_stored(&o), FP, &spec()).expect_err("refused");
        assert!(
            !reason.contains("evil") && !reason.contains("forged"),
            "{reason}"
        );
        let mut o = object(json!([]), json!(1));
        o["spec"]["target"]["name"] = json!("evil\u{202E}name");
        let reason = check_identity(&parse_stored(&o), FP, &spec()).expect_err("refused");
        assert!(!reason.contains("evil"), "{reason}");
    }

    // --- Failures -----------------------------------------------------------------------------

    #[test]
    fn a_409_is_a_conflict_and_everything_else_an_error() {
        assert_eq!(failure_outcome(Some(409)), "conflict");
        for code in [
            None,
            Some(400),
            Some(403),
            Some(404),
            Some(410),
            Some(422),
            Some(500),
        ] {
            assert_eq!(failure_outcome(code), "error", "{code:?}");
        }
        assert_eq!(
            CRD_MISSING,
            "the CustomResourceDefinition explanations.kausa.fedeam.com is not installed \
             (chart value history.installCRD)"
        );
        assert_eq!(
            CONFLICT,
            "409 Conflict: the Explanation was created or changed by another writer since it \
             was read; not retried"
        );
    }

    // --- Kausa-shaped objects -----------------------------------------------------------------

    #[test]
    fn kausa_shaped_is_the_label_the_name_and_a_spec_that_parses() {
        let good = object(json!([]), json!(1));
        assert!(is_kausa_shaped(&good));
        let mut cases: Vec<(String, Value)> = Vec::new();
        for labels in [
            json!(null),
            json!({}),
            json!({LABEL: "Kausa"}),
            json!({"app": "kausa"}),
        ] {
            let mut o = good.clone();
            o["metadata"]["labels"] = labels.clone();
            cases.push((format!("labels {labels}"), o));
        }
        for bad_name in [
            "kausa.0123456789ABCDEF".to_owned(),
            "kausa.0123456789abcde".to_owned(),
            "kausa.0123456789abcdef0".to_owned(),
            "kausa.0123456789abcdeg".to_owned(),
            "kausa-0123456789abcdef".to_owned(),
            "kausa.0123456789abcdef.1".to_owned(),
            "x.kausa.0123456789abcdef".to_owned(),
            "kausa.".to_owned(),
            "api".to_owned(),
        ] {
            let mut o = good.clone();
            o["metadata"]["name"] = json!(bad_name);
            cases.push((format!("name {bad_name}"), o));
        }
        let mut o = good.clone();
        remove(&mut o["metadata"], "name");
        cases.push(("no name".into(), o));
        for bad in bad_specs() {
            let mut o = good.clone();
            o["spec"] = bad.clone();
            cases.push((format!("spec {bad}"), o));
        }
        for junk in [
            json!(null),
            json!([]),
            json!("kausa"),
            json!({"metadata": []}),
        ] {
            cases.push((format!("object {junk}"), junk));
        }
        for (case, o) in cases {
            assert!(!is_kausa_shaped(&o), "{case}");
        }
    }

    // --- The sweep's plan ---------------------------------------------------------------------

    fn fingerprint(i: usize) -> String {
        format!("{i:016x}")
    }

    /// A fresh time, later for a larger `i` (at most 86399), within the age.
    fn fresh(i: usize) -> String {
        format!(
            "2026-10-27T{:02}:{:02}:{:02}Z",
            i / 3600,
            i / 60 % 60,
            i % 60
        )
    }

    /// A listed, kausa-shaped object.
    fn listed(namespace: &str, i: usize, last: Option<&str>, created: Option<&str>) -> Value {
        let mut metadata = Map::new();
        metadata.insert("name".into(), json!(name(&fingerprint(i))));
        metadata.insert("namespace".into(), json!(namespace));
        metadata.insert(
            "resourceVersion".into(),
            json!(format!("rv-{namespace}-{i}")),
        );
        metadata.insert("labels".into(), json!({LABEL: "kausa"}));
        if let Some(created) = created {
            metadata.insert("creationTimestamp".into(), json!(created));
        }
        let mut status = json!({"occurrences": 1});
        if let Some(last) = last {
            status["lastTime"] = json!(last);
        }
        json!({
            "apiVersion": "kausa.fedeam.com/v1alpha1",
            "kind": "Explanation",
            "metadata": metadata,
            "spec": {
                "rule": "D001",
                "target": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "api"},
            },
            "status": status,
        })
    }

    fn at(namespace: &str, i: usize, time: &str) -> Value {
        listed(namespace, i, Some(time), Some("2026-10-27T00:00:00Z"))
    }

    fn delete(namespace: &str, i: usize, reason: Reason) -> Delete {
        Delete {
            namespace: namespace.into(),
            name: name(&fingerprint(i)),
            resource_version: format!("rv-{namespace}-{i}"),
            reason,
        }
    }

    const OLD: &str = "2026-10-01T00:00:00Z";

    #[test]
    fn the_reasons_are_labelled_age_and_cap() {
        assert_eq!(Reason::Age.label(), "age");
        assert_eq!(Reason::Cap.label(), "cap");
        assert_eq!(MAX_ENTRIES, 5);
        assert_eq!(MAX_OBJECTS, 500);
    }

    #[test]
    fn objects_past_the_age_are_deleted_as_their_page_arrives() {
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let deletes = plan.page(&[
            at("shop", 1, OLD),
            at("shop", 2, "2026-10-21T09:59:59Z"),
            at("shop", 3, CUTOFF),
            at("web", 4, &fresh(4)),
        ]);
        assert_eq!(
            deletes,
            [
                delete("shop", 1, Reason::Age),
                delete("shop", 2, Reason::Age)
            ]
        );
        // The others are held for the cap, and an object deleted by age is not.
        assert_eq!(plan.held(), 2);
    }

    #[test]
    fn objects_that_are_not_kausa_shaped_are_neither_counted_nor_deleted() {
        let mut unlabelled = at("shop", 1, OLD);
        unlabelled["metadata"]["labels"] = json!({});
        let mut other_name = at("shop", 2, OLD);
        other_name["metadata"]["name"] = json!("kausa.not-a-fingerprint");
        let mut bad_spec = at("shop", 3, OLD);
        bad_spec["spec"]["rule"] = json!("X001");
        let mut no_spec = at("shop", 4, OLD);
        remove(&mut no_spec, "spec");
        let junk = [
            unlabelled,
            other_name,
            bad_spec,
            no_spec,
            json!(null),
            json!([1]),
        ];
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        assert_eq!(plan.page(&junk), []);
        assert_eq!(plan.held(), 0);
        // MAX_OBJECTS kausa-shaped objects and the same junk, fresh: nothing goes for the cap.
        let mut fresh_junk = junk.to_vec();
        for o in &mut fresh_junk {
            if o.is_object() {
                o["status"]["lastTime"] = json!(fresh(0));
            }
        }
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let mut deletes = Vec::new();
        for chunk in (0..MAX_OBJECTS)
            .map(|i| at("shop", i, &fresh(i)))
            .collect::<Vec<_>>()
            .chunks(50)
        {
            deletes.extend(plan.page(chunk));
        }
        deletes.extend(plan.page(&fresh_junk));
        assert_eq!(deletes, []);
        assert_eq!(plan.held(), MAX_OBJECTS);
    }

    #[test]
    fn the_creation_time_stands_in_for_a_status_time_that_is_unreadable_or_later_than_now() {
        let created_old = Some(OLD);
        let created_fresh = Some("2026-10-27T00:00:00Z");
        let cases = [
            // (status.lastTime, creationTimestamp, deleted by age)
            (None, created_old, true),
            (Some("yesterday"), created_old, true),
            (Some("2026-10-27T10:00:00.5Z"), created_old, true),
            (Some("2026-10-28T10:00:01Z"), created_old, true),
            (Some("2099-01-01T00:00:00Z"), created_old, true),
            (Some("2099-01-01T00:00:00Z"), created_fresh, false),
            (None, created_fresh, false),
            (Some("2026-10-27T10:00:00Z"), created_old, false),
            (Some(OLD), created_fresh, true),
        ];
        for (i, (last, created, deleted)) in cases.into_iter().enumerate() {
            let mut plan = SweepPlan::new(NOW, CUTOFF);
            let deletes = plan.page(&[listed("shop", i, last, created)]);
            if deleted {
                assert_eq!(
                    deletes,
                    [delete("shop", i, Reason::Age)],
                    "{last:?} {created:?}"
                );
                assert_eq!(plan.held(), 0, "{last:?} {created:?}");
            } else {
                assert_eq!(deletes, [], "{last:?} {created:?}");
                assert_eq!(plan.held(), 1, "{last:?} {created:?}");
            }
        }
        // Neither time readable: skipped, neither counted nor deleted.
        for created in [None, Some("yesterday"), Some("2026-10-01")] {
            let mut plan = SweepPlan::new(NOW, CUTOFF);
            assert_eq!(plan.page(&[listed("shop", 1, Some("x"), created)]), []);
            assert_eq!(plan.held(), 0, "{created:?}");
        }
    }

    /// A permutation of `0..n`, so that the oldest objects do not arrive first.
    fn shuffled(n: usize) -> Vec<usize> {
        (0..n).map(|i| (i * 7919) % n).collect()
    }

    #[test]
    fn the_oldest_beyond_the_cap_are_deleted_and_the_heap_stays_bounded() {
        let n = MAX_OBJECTS + 5;
        let objects: Vec<Value> = shuffled(n)
            .into_iter()
            .map(|i| at("shop", i, &fresh(i)))
            .collect();
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let mut deletes = Vec::new();
        for chunk in objects.chunks(50) {
            deletes.extend(plan.page(chunk));
            assert!(plan.held() <= MAX_OBJECTS, "{}", plan.held());
        }
        assert_eq!(plan.held(), MAX_OBJECTS);
        let deleted: BTreeSet<String> = deletes
            .iter()
            .inspect(|d| assert_eq!(d.reason, Reason::Cap, "{d:?}"))
            .map(|d| d.name.clone())
            .collect();
        let oldest: BTreeSet<String> = (0..5).map(|i| name(&fingerprint(i))).collect();
        assert_eq!(deleted, oldest);
        assert_eq!(deletes.len(), 5);
        for d in &deletes {
            let i = (0..5)
                .find(|i| name(&fingerprint(*i)) == d.name)
                .expect("one of the oldest");
            assert_eq!(d, &delete("shop", i, Reason::Cap));
        }
    }

    #[test]
    fn a_tuple_popped_for_the_cap_is_deleted_even_if_the_list_stops_there() {
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let first: Vec<Value> = (1..=MAX_OBJECTS)
            .map(|i| at("shop", i, &fresh(i)))
            .collect();
        let mut deletes = Vec::new();
        for chunk in first.chunks(50) {
            deletes.extend(plan.page(chunk));
        }
        assert_eq!(deletes, []);
        // One page more, older than everything held: it is the one beyond the cap, deleted now,
        // whether or not the list goes on.
        assert_eq!(
            plan.page(&[at("shop", 0, &fresh(0))]),
            [delete("shop", 0, Reason::Cap)]
        );
        // One newer: the oldest held goes.
        assert_eq!(
            plan.page(&[at("shop", 9999, &fresh(9999))]),
            [delete("shop", 1, Reason::Cap)]
        );
        assert_eq!(plan.held(), MAX_OBJECTS);
    }

    #[test]
    fn age_comes_before_the_cap() {
        let mut objects: Vec<Value> = (0..MAX_OBJECTS).map(|i| at("shop", i, &fresh(i))).collect();
        objects.extend((MAX_OBJECTS..MAX_OBJECTS + 10).map(|i| at("shop", i, OLD)));
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let mut deletes = Vec::new();
        for chunk in objects.chunks(50) {
            deletes.extend(plan.page(chunk));
        }
        assert_eq!(deletes.len(), 10);
        assert!(
            deletes.iter().all(|d| d.reason == Reason::Age),
            "{deletes:?}"
        );
        assert_eq!(plan.held(), MAX_OBJECTS);
    }

    #[test]
    fn ties_go_to_the_smaller_namespace_then_the_smaller_name() {
        let time = fresh(100);
        // One object per namespace, all at the same time, the smallest namespace last.
        let objects: Vec<Value> = (0..=MAX_OBJECTS)
            .rev()
            .map(|i| at(&format!("ns-{i:03}"), i, &time))
            .collect();
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let deletes: Vec<Delete> = objects.chunks(50).flat_map(|c| plan.page(c)).collect();
        assert_eq!(deletes, [delete("ns-000", 0, Reason::Cap)]);
        // One namespace: the smaller name.
        let objects: Vec<Value> = (0..=MAX_OBJECTS)
            .rev()
            .map(|i| at("shop", i, &time))
            .collect();
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let deletes: Vec<Delete> = objects.chunks(50).flat_map(|c| plan.page(c)).collect();
        assert_eq!(deletes, [delete("shop", 0, Reason::Cap)]);
        // The namespace decides before the name.
        let mut objects: Vec<Value> = (1..MAX_OBJECTS).map(|i| at("m", i, &time)).collect();
        objects.push(at("z", 0, &time));
        objects.push(at("a", MAX_OBJECTS, &time));
        let mut plan = SweepPlan::new(NOW, CUTOFF);
        let deletes: Vec<Delete> = objects.chunks(50).flat_map(|c| plan.page(c)).collect();
        assert_eq!(deletes, [delete("a", MAX_OBJECTS, Reason::Cap)]);
    }

    // --- The schema file, shared with the chart -----------------------------------------------

    fn schema() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/charts/kausa/files/explanation-schema.json"
        );
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path} is not JSON: {e}"))
    }

    fn required(schema: &Value) -> BTreeSet<String> {
        schema["required"]
            .as_array()
            .map(|r| {
                r.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn max_length(schema: &Value) -> Option<u64> {
        schema["maxLength"].as_u64()
    }

    #[test]
    fn the_schema_requires_the_identity_and_every_entry_field() {
        let s = schema();
        assert_eq!(s["type"], "object");
        assert_eq!(required(&s), set(&["spec"]));
        for key in ["apiVersion", "kind", "metadata"] {
            assert!(s["properties"].get(key).is_none(), "{key} is listed");
        }
        assert_eq!(keys(&s["properties"]), set(&["spec", "status"]));
        let spec = &s["properties"]["spec"];
        assert_eq!(spec["type"], "object");
        assert_eq!(required(spec), set(&["rule", "target"]));
        assert_eq!(
            spec["x-kubernetes-validations"],
            json!([{"rule": "self == oldSelf", "message": "spec is immutable"}])
        );
        let target = &spec["properties"]["target"];
        assert_eq!(required(target), set(&["apiVersion", "kind", "name"]));
        let entry = &s["properties"]["status"]["properties"]["entries"]["items"];
        assert_eq!(entry["type"], "object");
        assert_eq!(required(entry), set(&ENTRY_FIELDS));
        assert_eq!(keys(&entry["properties"]), set(&ENTRY_FIELDS));
        assert_eq!(
            keys(&s["properties"]["status"]["properties"]),
            set(&["occurrences", "lastTime", "entries"])
        );
    }

    #[test]
    fn the_schema_limits_equal_kausa_s_caps() {
        let s = schema();
        let spec = &s["properties"]["spec"]["properties"];
        assert_eq!(spec["rule"]["type"], "string");
        assert_eq!(spec["rule"]["pattern"], "^D[0-9]{3}$");
        assert_eq!(max_length(&spec["rule"]), Some(4));
        let target = &spec["target"]["properties"];
        assert_eq!(max_length(&target["apiVersion"]), Some(253));
        assert_eq!(max_length(&target["kind"]), Some(63));
        assert_eq!(max_length(&target["name"]), Some(253));
        let status = &s["properties"]["status"]["properties"];
        assert_eq!(status["occurrences"]["type"], "integer");
        assert_eq!(status["occurrences"]["minimum"].as_u64(), Some(1));
        let entries = &status["entries"];
        assert_eq!(entries["type"], "array");
        assert_eq!(entries["maxItems"].as_u64(), Some(MAX_ENTRIES as u64));
        let entry = &entries["items"]["properties"];
        assert_eq!(max_length(&entry["summary"]), Some(MAX_SUMMARY as u64));
        assert_eq!(max_length(&entry["cause"]), Some(MAX_CAUSE as u64));
        assert_eq!(entry["steps"]["type"], "array");
        assert_eq!(entry["steps"]["maxItems"].as_u64(), Some(MAX_STEPS as u64));
        assert_eq!(entry["steps"]["items"]["type"], "string");
        assert_eq!(max_length(&entry["steps"]["items"]), Some(MAX_STEP as u64));
        assert_eq!(entry["confidence"]["type"], "string");
        assert_eq!(
            entry["confidence"]["enum"],
            json!(["low", "medium", "high"])
        );
        assert_eq!(max_length(&entry["model"]), Some(253));
        assert_eq!(max_length(&entry["promptVersion"]), Some(16));
        assert!(PROMPT_VERSION.chars().count() <= 16);
        assert_eq!(entry["count"]["type"], "integer");
        assert_eq!(entry["count"]["minimum"].as_u64(), Some(1));
        for time in [&status["lastTime"], &entry["firstTime"], &entry["lastTime"]] {
            assert_eq!(time["type"], "string");
            assert_eq!(time["format"], "date-time");
            assert_eq!(max_length(time), Some(20));
        }
        // Every string is capped: by a maxLength, or by an enum.
        fn strings_capped(schema: &Value, path: &str) {
            if schema["type"] == "string" && schema.get("enum").is_none() {
                assert!(max_length(schema).is_some(), "{path} has no maxLength");
            }
            if let Some(p) = schema["properties"].as_object() {
                for (k, v) in p {
                    strings_capped(v, &format!("{path}.{k}"));
                }
            }
            if schema.get("items").is_some() {
                strings_capped(&schema["items"], &format!("{path}[]"));
            }
        }
        strings_capped(&s, "");
    }

    #[test]
    fn every_property_of_the_schema_is_described() {
        fn described(schema: &Value, path: &str) {
            if let Some(p) = schema["properties"].as_object() {
                for (k, v) in p {
                    let here = format!("{path}.{k}");
                    assert!(
                        v["description"]
                            .as_str()
                            .is_some_and(|d| !d.trim().is_empty()),
                        "{here} has no description"
                    );
                    described(v, &here);
                }
            }
            if schema.get("items").is_some() {
                described(&schema["items"], &format!("{path}[]"));
            }
        }
        described(&schema(), "");
    }

    /// Checks `value` against `schema` for what kausa relies on: only listed properties, the
    /// required ones present, types, caps in characters, enums and minimums.
    fn conforms(value: &Value, schema: &Value, path: &str) {
        match schema["type"].as_str() {
            Some("object") => {
                let object = value
                    .as_object()
                    .unwrap_or_else(|| panic!("{path}: not an object"));
                let properties = schema["properties"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                for (k, v) in object {
                    let sub = properties
                        .get(k)
                        .unwrap_or_else(|| panic!("{path}.{k} is not in the schema"));
                    conforms(v, sub, &format!("{path}.{k}"));
                }
                for r in required(schema) {
                    assert!(object.contains_key(&r), "{path}.{r} is required");
                }
            }
            Some("array") => {
                let items = value
                    .as_array()
                    .unwrap_or_else(|| panic!("{path}: not an array"));
                if let Some(max) = schema["maxItems"].as_u64() {
                    assert!(items.len() as u64 <= max, "{path}: more than {max} items");
                }
                for (i, item) in items.iter().enumerate() {
                    conforms(item, &schema["items"], &format!("{path}[{i}]"));
                }
            }
            Some("string") => {
                let text = value
                    .as_str()
                    .unwrap_or_else(|| panic!("{path}: not a string"));
                if let Some(max) = max_length(schema) {
                    assert!(text.chars().count() as u64 <= max, "{path}: above {max}");
                }
                if let Some(allowed) = schema["enum"].as_array() {
                    assert!(allowed.contains(value), "{path}: {value} is not allowed");
                }
            }
            Some("integer") => {
                let n = value
                    .as_u64()
                    .unwrap_or_else(|| panic!("{path}: not an integer"));
                if let Some(min) = schema["minimum"].as_u64() {
                    assert!(n >= min, "{path}: below {min}");
                }
            }
            other => panic!("{path}: unexpected schema type {other:?}"),
        }
    }

    #[test]
    fn what_kausa_writes_uses_only_the_schema_s_properties() {
        let s = schema();
        let long = Explanation {
            summary: "é".repeat(MAX_SUMMARY),
            cause: "é".repeat(MAX_CAUSE),
            steps: vec!["é".repeat(MAX_STEP); MAX_STEPS],
            confidence: "low".into(),
        };
        let body = create_body(FP, &spec(), &long, &"é".repeat(300), NOW);
        let mut object = body.clone();
        for key in ["apiVersion", "kind", "metadata"] {
            remove(&mut object, key);
        }
        conforms(&object, &s, "create");
        let stored = object_with_entries();
        let status = merge(&parse_stored(&stored), &long, "m", NOW, CUTOFF);
        let patch = patch_body("41", &status);
        conforms(
            &json!({"spec": body["spec"], "status": patch["status"]}),
            &s,
            "patch",
        );
    }

    fn object_with_entries() -> Value {
        object(
            json!(
                (1..=5)
                    .map(|i| {
                        let t = format!("2026-10-27T0{i}:00:00Z");
                        entry(&format!("s{i}"), &t, &t, i)
                    })
                    .collect::<Vec<_>>()
            ),
            json!(15),
        )
    }
}
