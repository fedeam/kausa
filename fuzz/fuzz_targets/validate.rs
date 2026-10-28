//! Validation (src/prompt.rs) of any answer a model could return: it must never panic.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/prompt.rs"]
mod prompt;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|answer: &str| {
    let _ = prompt::validate(answer);
});
