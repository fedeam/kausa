//! Validation (src/prompt.rs) of any answer a model could return: it must never panic. On a valid
//! answer, withholding (docs/adr/0025-withhold-destructive-commands.md) must leave no command shape
//! in any text, lower the confidence of an answer that carries the notice, and be its own result:
//! a second pass returns the answer as it is, with no ids. A name mapped back that completes a
//! command in one version (here `job-1` restored as `kubectl`) is withheld in every version, and
//! the versions keep the same number of steps.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/prompt.rs"]
mod prompt;

use libfuzzer_sys::fuzz_target;

use prompt::Explanation;

/// Whether no text of `e` matches a shape of ADR 0025.
fn no_shape(e: &Explanation) -> bool {
    prompt::shapes(&e.summary).is_empty()
        && prompt::shapes(&e.cause).is_empty()
        && e.steps.iter().all(|s| prompt::shapes(s).is_empty())
}

fuzz_target!(|answer: &str| {
    let Ok(e) = prompt::validate(answer) else {
        return;
    };

    let (w, _) = prompt::withhold(e.clone());
    assert!(no_shape(&w), "a shape survives withhold");
    assert!(
        !w.withheld() || w.confidence == "low",
        "a withheld answer keeps its confidence"
    );
    let (again, ids) = prompt::withhold(w.clone());
    assert_eq!(again, w, "withhold is not idempotent");
    assert!(ids.is_empty(), "a second pass finds a shape");

    let restored = e.map_clean(|t| t.replace("job-1", "kubectl"));
    let (versions, _) = prompt::withhold_all(vec![e, restored]);
    assert_eq!(
        versions.len(),
        2,
        "withhold_all changes the number of versions"
    );
    assert!(
        versions.iter().all(no_shape),
        "a shape survives withhold_all"
    );
    assert_eq!(
        versions[0].steps.len(),
        versions[1].steps.len(),
        "the versions differ in their number of steps"
    );
});
