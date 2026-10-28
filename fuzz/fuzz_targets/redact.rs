//! Redaction (src/redact.rs) of any text a cluster could produce: it must never panic, and
//! restoring the placeholders of redacted text must not panic either.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/redact.rs"]
mod redact;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let mut redactor = redact::Redactor::new();
    let redacted = redactor.text(text);
    let _ = redactor.restore(&redacted);
});
