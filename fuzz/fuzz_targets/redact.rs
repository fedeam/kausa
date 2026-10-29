//! Redaction (src/redact.rs) of any text a cluster could produce: it must never panic, and
//! restoring the placeholders of redacted text, fully or only for the namespace and the owner (the
//! narrow restore of a notification, docs/adr/0015-notifications.md), must not panic either.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/redact.rs"]
mod redact;

use libfuzzer_sys::fuzz_target;
use redact::NameKind;

fuzz_target!(|text: &str| {
    let mut redactor = redact::Redactor::new();
    // The first two lines name the namespace and the owner, as `context::build` registers them.
    let mut lines = text.lines();
    let namespace = lines.next().unwrap_or_default();
    let owner = lines.next().unwrap_or_default();
    redactor.name(NameKind::Namespace, namespace);
    redactor.name(NameKind::Object("Deployment"), owner);
    let redacted = redactor.text(text);
    let _ = redactor.restore(&redacted);
    let _ = redactor.restore_only(&redacted, &[namespace, owner]);
});
