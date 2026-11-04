//! `kausa check-endpoint` (docs/adr/0020-check-the-endpoint.md): one fixed, fabricated request to
//! the model endpoint, on demand, with no Kubernetes client and no budget, and one `endpoint check`
//! line that says which step failed and why.

use std::collections::BTreeSet;
use std::io;
use std::time::Instant;

use k8s_openapi::jiff::Timestamp;
use serde_json::{Value, json};

use crate::context::{self, Gathered};
use crate::detect::Detection;
use crate::incident::{Incident, IncidentKey, Signal};
use crate::log;
use crate::model::{CallError, Cause, INVALID_ANSWER_HINT, ModelClient};
use crate::prompt::{self, OutputMode};
use crate::record::{ContainerRecord, EventRecord, OwnerRef, PodRecord, Regarding};

/// The names of the fabricated incident. None of them is sent in clear: redaction turns each into
/// a placeholder, as it does with a real incident's names.
const NAMESPACE: &str = "example";
const OWNER: &str = "example-app";
const POD: &str = "example-app-5d9c7b6f4-x2k8p";
const CONTAINER: &str = "app";
const IMAGE: &str = "registry.example.com/example/app:1.0";
const EVENT_NOTE: &str =
    "Back-off restarting failed container app in pod example-app-5d9c7b6f4-x2k8p";
const LOG_TAIL: &str = "synthetic failure from kausa check-endpoint\nerror: EXAMPLE_URL is not set";

/// The fabricated `D001` crash loop the check explains: one Deployment, one Pod, one container,
/// one `BackOff` event and a log tail that says it is synthetic. Every timestamp is the Unix
/// epoch and the UIDs are empty; nothing of it comes from a cluster.
fn incident() -> Gathered {
    let owner = OwnerRef {
        api_version: "apps/v1".into(),
        kind: "Deployment".into(),
        name: OWNER.into(),
        uid: String::new(),
    };
    let key = IncidentKey {
        namespace: NAMESPACE.into(),
        owner_kind: owner.kind.clone(),
        owner_name: owner.name.clone(),
        detection: Detection::CrashLoop,
    };
    let sample = Signal {
        key: key.clone(),
        owner: owner.clone(),
        observed_at: Timestamp::UNIX_EPOCH,
        event: None,
        regarding_kind: None,
        pod: Some(POD.into()),
        container: Some(CONTAINER.into()),
        image: Some(IMAGE.into()),
        reason: "CrashLoopBackOff".into(),
        message: String::new(),
        exit_code: Some(1),
    };
    Gathered {
        incident: Incident {
            key,
            owner,
            fingerprint: String::new(),
            first_seen: Timestamp::UNIX_EPOCH,
            last_seen: Timestamp::UNIX_EPOCH,
            pods: BTreeSet::from([POD.to_owned()]),
            sample,
        },
        pods: vec![PodRecord {
            namespace: NAMESPACE.into(),
            name: POD.into(),
            phase: Some("Running".into()),
            containers: vec![ContainerRecord {
                name: CONTAINER.into(),
                image: Some(IMAGE.into()),
                waiting_reason: Some("CrashLoopBackOff".into()),
                last_termination_reason: Some("Error".into()),
                last_termination_exit_code: Some(1),
                restart_count: 5,
                ..Default::default()
            }],
            ..Default::default()
        }],
        events: vec![EventRecord {
            uid: String::new(),
            reason: "BackOff".into(),
            note: Some(EVENT_NOTE.into()),
            regarding: Regarding {
                api_version: "v1".into(),
                kind: "Pod".into(),
                namespace: NAMESPACE.into(),
                name: POD.into(),
                uid: None,
            },
            reporting_controller: None,
            first_seen: Some(Timestamp::UNIX_EPOCH),
            last_seen: Some(Timestamp::UNIX_EPOCH),
            count: 5,
        }],
        pods_total: 1,
        log_tail: Some(LOG_TAIL.into()),
        rollout: None,
    }
}

/// The body the check sends: the fabricated incident through the same redaction and request as a
/// real one, with the output cap of `kausa run`. The fixed request is small, so it is not fitted
/// to `--max-input-tokens`.
pub fn request(model: &str, mode: OutputMode) -> Value {
    let gathered = incident();
    let (context, _) = context::build(&gathered);
    prompt::request(
        gathered.incident.key.detection,
        model,
        &context,
        mode,
        prompt::MAX_TOKENS,
    )
}

/// What was checked, beside the answer: the fields of the line that do not depend on it.
pub struct Target<'a> {
    /// The `--endpoint` value as given; only its scheme, host and port are shown.
    pub endpoint: &'a str,
    pub model: &'a str,
    pub mode: OutputMode,
    pub api_key: bool,
}

/// How the one call ended.
enum Outcome {
    /// A chat completion that passed `prompt::validate`.
    Passed {
        status: u16,
        tokens_in: Option<u64>,
        tokens_out: Option<u64>,
    },
    /// A chat completion that failed `prompt::validate`: `invalid_answer`.
    Invalid { status: u16, error: String },
    /// No chat completion with content.
    Failed(CallError),
}

/// Sends the one request, writes the one `endpoint check` line and says whether the check
/// passed. Never retries and never waits for a `Retry-After`. The error is the log's: stdout
/// cannot be written.
pub async fn run(client: &ModelClient, target: &Target<'_>) -> io::Result<bool> {
    let body = request(target.model, target.mode);
    let start = Instant::now();
    let outcome = match client.complete(&body).await {
        Ok(answer) => match prompt::validate(&answer.content) {
            Ok(_) => Outcome::Passed {
                status: answer.status,
                tokens_in: answer.prompt_tokens,
                tokens_out: answer.completion_tokens,
            },
            Err(error) => Outcome::Invalid {
                status: answer.status,
                error,
            },
        },
        Err(error) => Outcome::Failed(error),
    };
    let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let passed = matches!(outcome, Outcome::Passed { .. });
    let json_schema = target.mode == OutputMode::JsonSchema;
    let mut fields = line(&outcome, json_schema);
    fields["json_schema"] = Value::from(json_schema);
    fields["endpoint"] = origin(target.endpoint).map_or(Value::Null, Value::from);
    fields["model"] = Value::from(target.model);
    fields["timeout_seconds"] = Value::from(client.timeout().as_secs());
    fields["api_key"] = Value::from(target.api_key);
    fields["latency_ms"] = Value::from(latency_ms);
    fields["prompt_version"] = Value::from(prompt::PROMPT_VERSION);
    if passed {
        log::info("endpoint check", fields)?;
    } else {
        log::error("endpoint check", fields)?;
    }
    Ok(passed)
}

/// The fields of the line that depend on the outcome. A step is `true` only when the answer
/// proves it, `false` for the step that failed and `null` when the answer says nothing either
/// way. Never the answer's text.
fn line(outcome: &Outcome, json_schema: bool) -> Value {
    let (status, cause) = match outcome {
        Outcome::Passed { status, .. } | Outcome::Invalid { status, .. } => (Some(*status), None),
        Outcome::Failed(error) => (error.status, Some(error.cause)),
    };
    // Any 2xx proves the key, the model and the request.
    let proven = status.is_some_and(|s| (200..=299).contains(&s));
    let step = |failed: bool| {
        if failed {
            Value::Bool(false)
        } else if proven {
            Value::Bool(true)
        } else {
            Value::Null
        }
    };
    let answer_valid = match outcome {
        Outcome::Passed { .. } => Value::Bool(true),
        Outcome::Invalid { .. } => Value::Bool(false),
        Outcome::Failed(error) => match error.cause {
            Cause::NoContent(_) | Cause::NotAnAnswer => Value::Bool(false),
            _ => Value::Null,
        },
    };
    let mut fields = json!({
        "result": if matches!(outcome, Outcome::Passed { .. }) { "ok" } else { "failed" },
        "status": status,
        "connected": cause != Some(Cause::Connection),
        "authenticated": step(cause == Some(Cause::Key)),
        "model_found": step(cause == Some(Cause::NotFound)),
        "request_accepted": step(matches!(cause, Some(Cause::Rejected | Cause::TooLarge))),
        "answer_valid": answer_valid,
        "cause": null,
        "class": null,
        "reason": null,
        "hint": null,
        "detail": null,
        "retry_after_seconds": null,
        "tokens_in": null,
        "tokens_out": null,
    });
    match outcome {
        Outcome::Passed {
            tokens_in,
            tokens_out,
            ..
        } => {
            fields["tokens_in"] = json!(tokens_in);
            fields["tokens_out"] = json!(tokens_out);
        }
        Outcome::Invalid { error, .. } => {
            fields["cause"] = Value::from("invalid_answer");
            fields["reason"] = Value::from(format!("invalid answer: {error}"));
            fields["hint"] = Value::from(INVALID_ANSWER_HINT);
        }
        Outcome::Failed(error) => {
            fields["cause"] = Value::from(error.cause.as_str());
            fields["class"] = Value::from(error.cause.class().as_str());
            fields["reason"] = Value::from(error.message.as_str());
            fields["hint"] = Value::from(error.cause.hint(json_schema));
            fields["detail"] = json!(error.detail);
            fields["retry_after_seconds"] = json!(error.retry_after.map(|d| d.as_secs()));
        }
    }
    fields
}

/// The scheme, host and port of an endpoint (`https://api.example.com:443`), the port written
/// even when it is the scheme's default: never its user information, path or query. `None` when
/// it is not a URL.
fn origin(endpoint: &str) -> Option<String> {
    let url = reqwest::Url::parse(endpoint).ok()?;
    // `host_str` writes an IPv6 address in brackets.
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    Some(format!("{}://{host}:{port}", url.scheme()))
}

#[cfg(test)]
mod tests {
    // Acceptance tests for docs/adr/0020-check-the-endpoint.md, written before the code. They use
    // `fn request(model: &str, mode: OutputMode) -> serde_json::Value`, the body the check sends:
    // the constant fabricated `D001` incident through `context::build` and `prompt::request` with
    // `prompt::MAX_TOKENS`. The golden files pin it; tests/cli.rs checks that the body a local
    // server receives is the same.
    //
    // The golden files were built from this incident, as ADR 0020 and its design describe it:
    // namespace `example`; owner `apps/v1` `Deployment` `example-app`, empty UID; one Pod
    // `example-app-5d9c7b6f4-x2k8p` in phase `Running` (so `pods_affected` and `pods_total` are
    // 1), no node, no service account; one container `app`, image
    // `registry.example.com/example/app:1.0`, waiting `CrashLoopBackOff` with no message, last
    // termination `Error` with exit code 1, 5 restarts, no resources and no probes; one Warning
    // event `BackOff` with the note `Back-off restarting failed container app in pod
    // example-app-5d9c7b6f4-x2k8p` and a count of 5; the log tail `synthetic failure from kausa
    // check-endpoint\nerror: EXAMPLE_URL is not set` (no final newline). Redaction turns the
    // `EXAMPLE` of the log line into `namespace-1`, as it would in a real log.

    use super::*;

    use serde_json::Value;

    use crate::prompt::OutputMode;

    fn golden(text: &str) -> Value {
        serde_json::from_str(text).expect("a golden file is JSON")
    }

    #[test]
    fn the_request_is_the_golden_body_in_both_output_modes() {
        assert_eq!(
            request("example-model", OutputMode::JsonSchema),
            golden(include_str!(
                "../tests/golden/check-endpoint-request-json-schema.json"
            )),
            "the JSON-schema body differs from tests/golden"
        );
        assert_eq!(
            request("example-model", OutputMode::Instructions),
            golden(include_str!(
                "../tests/golden/check-endpoint-request-instructions.json"
            )),
            "the instructions body differs from tests/golden"
        );
    }

    #[test]
    fn the_request_carries_the_model_named_and_the_shared_output_cap() {
        let body = request("another-model", OutputMode::JsonSchema);
        assert_eq!(body["model"], "another-model");
        assert_eq!(body["max_tokens"], crate::prompt::MAX_TOKENS);
        assert!(body.get("response_format").is_some());
        let body = request("another-model", OutputMode::Instructions);
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn the_request_names_no_fabricated_name_in_clear() {
        // Every name of the fabricated incident is a placeholder in what is sent.
        for mode in [OutputMode::JsonSchema, OutputMode::Instructions] {
            let text = request("example-model", mode).to_string();
            for name in [
                "example-app",
                "x2k8p",
                "registry.example.com",
                "EXAMPLE_URL",
            ] {
                assert!(!text.contains(name), "{name} is sent in clear");
            }
            assert!(text.contains("D001"));
            assert!(text.contains("synthetic failure from kausa check-endpoint"));
        }
    }

    #[test]
    fn the_endpoint_shows_only_its_scheme_host_and_port() {
        for (endpoint, shown) in [
            (
                "https://api.example.com/v1",
                Some("https://api.example.com:443"),
            ),
            ("http://127.0.0.1:11434/v1", Some("http://127.0.0.1:11434")),
            (
                "http://model.example.com/v1/",
                Some("http://model.example.com:80"),
            ),
            (
                "https://[2001:db8::1]:8443/v1",
                Some("https://[2001:db8::1]:8443"),
            ),
            (
                "https://user:secret@api.example.com/v1?key=secret#secret",
                Some("https://api.example.com:443"),
            ),
            ("https://", None),
            ("not a url", None),
        ] {
            assert_eq!(origin(endpoint).as_deref(), shown, "{endpoint}");
        }
    }

    fn failed(cause: Cause, status: Option<u16>) -> Outcome {
        Outcome::Failed(CallError {
            message: "the endpoint answered".into(),
            detail: status.map(|_| "{}".into()),
            cause,
            status,
            retry_after: None,
            reached: cause != Cause::Connection,
        })
    }

    #[test]
    fn each_outcome_proves_only_the_steps_its_answer_shows() {
        use crate::model::Missing;
        let (t, f, n) = (Value::Bool(true), Value::Bool(false), Value::Null);
        // connected, authenticated, model_found, request_accepted, answer_valid
        let cases = [
            (failed(Cause::Connection, None), [&f, &n, &n, &n, &n]),
            (failed(Cause::Timeout, None), [&t, &n, &n, &n, &n]),
            (failed(Cause::Timeout, Some(408)), [&t, &n, &n, &n, &n]),
            (failed(Cause::Redirect, Some(307)), [&t, &n, &n, &n, &n]),
            (failed(Cause::RateLimited, Some(429)), [&t, &n, &n, &n, &n]),
            (failed(Cause::Quota, Some(402)), [&t, &n, &n, &n, &n]),
            (failed(Cause::Key, Some(403)), [&t, &f, &n, &n, &n]),
            (failed(Cause::NotFound, Some(405)), [&t, &n, &f, &n, &n]),
            (failed(Cause::TooLarge, Some(413)), [&t, &n, &n, &f, &n]),
            (failed(Cause::Rejected, Some(422)), [&t, &n, &n, &f, &n]),
            (failed(Cause::ServerError, Some(500)), [&t, &n, &n, &n, &n]),
            (failed(Cause::NotAnAnswer, Some(200)), [&t, &t, &t, &t, &f]),
            (
                failed(Cause::NoContent(Missing::Refusal), Some(201)),
                [&t, &t, &t, &t, &f],
            ),
            (
                Outcome::Invalid {
                    status: 200,
                    error: "the answer is not JSON".into(),
                },
                [&t, &t, &t, &t, &f],
            ),
            (
                Outcome::Passed {
                    status: 200,
                    tokens_in: Some(1),
                    tokens_out: None,
                },
                [&t, &t, &t, &t, &t],
            ),
        ];
        for (outcome, expected) in cases {
            let line = line(&outcome, true);
            let steps = [
                "connected",
                "authenticated",
                "model_found",
                "request_accepted",
                "answer_valid",
            ]
            .map(|step| &line[step]);
            assert_eq!(steps, expected, "{line}");
        }
    }

    #[test]
    fn a_failure_names_its_cause_class_and_hint_and_reports_no_tokens() {
        let line = line(&failed(Cause::Rejected, Some(400)), false);
        assert_eq!(line["result"], "failed");
        assert_eq!(line["cause"], "rejected");
        assert_eq!(line["class"], "rejected");
        assert_eq!(line["hint"], Cause::Rejected.hint(false));
        assert_eq!(line["detail"], "{}");
        assert_eq!(line["tokens_in"], Value::Null);
        let line = super::line(
            &Outcome::Invalid {
                status: 200,
                error: "the answer is not JSON".into(),
            },
            true,
        );
        assert_eq!(line["cause"], "invalid_answer");
        assert_eq!(line["class"], Value::Null);
        assert_eq!(line["reason"], "invalid answer: the answer is not JSON");
        assert_eq!(line["hint"], INVALID_ANSWER_HINT);
        assert_eq!(line["detail"], Value::Null);
        let line = super::line(
            &Outcome::Passed {
                status: 200,
                tokens_in: Some(120),
                tokens_out: None,
            },
            true,
        );
        assert_eq!(line["result"], "ok");
        assert_eq!(
            (&line["tokens_in"], &line["tokens_out"]),
            (&json!(120), &Value::Null)
        );
        for field in [
            "cause",
            "class",
            "reason",
            "hint",
            "detail",
            "retry_after_seconds",
        ] {
            assert_eq!(line[field], Value::Null, "{field}");
        }
    }
}
