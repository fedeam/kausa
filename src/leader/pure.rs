//! The pure part of the leader election (docs/adr/0019-high-availability.md): what kausa reads from
//! a stored, untrusted Lease, when a standby may take it, and the bodies of the three merge
//! patches. No I/O and no clock of its own: every `Instant` and `Timestamp` comes in.

use std::time::{Duration, Instant};

use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use k8s_openapi::jiff::Timestamp;
use serde_json::{Value, json};

/// The election's timings. Production uses [`TIMINGS`]; tests use the same ratios, shorter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    /// A standby takes the Lease once it has seen it unchanged this long.
    pub lease_duration: Duration,
    /// The leader acts until this long after it *sent* its last good renewal.
    pub renew_deadline: Duration,
    /// The leader renews and a standby reads this often.
    pub retry_period: Duration,
    /// The bound of each Lease call.
    pub call_timeout: Duration,
    /// The bound of the one release at shutdown.
    pub release_timeout: Duration,
}

/// The decided timings (docs/adr/0019-high-availability.md): constants, not flags.
pub const TIMINGS: Timings = Timings {
    lease_duration: Duration::from_secs(30),
    renew_deadline: Duration::from_secs(20),
    retry_period: Duration::from_secs(5),
    call_timeout: Duration::from_secs(5),
    release_timeout: Duration::from_secs(2),
};

/// The longest a cluster write or a webhook attempt may run (history, webhook and Event writes).
pub const WRITE_BOUND: Duration = Duration::from_secs(10);

// Every write a leader starts within its term ends before a standby may take the Lease over.
const _: () = assert!(
    TIMINGS.renew_deadline.as_nanos() + WRITE_BOUND.as_nanos() <= TIMINGS.lease_duration.as_nanos()
);

/// What kausa takes from a stored, untrusted Lease: its version and its holder. Nothing else of it
/// is ever read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// `metadata.resourceVersion`, echoed as the precondition of the next patch and compared.
    pub version: String,
    /// `spec.holderIdentity`; `None` when the spec or the field is absent, or it is `""` (free).
    pub holder: Option<String>,
}

/// The version and holder of `lease`, or `None` without a `metadata.resourceVersion`: an empty one
/// too, because an empty precondition would make the next patch unconditional.
pub fn observe(lease: &Lease) -> Option<Observed> {
    let version = lease
        .metadata
        .resource_version
        .as_deref()
        .filter(|v| !v.is_empty())?
        .to_owned();
    let holder = lease
        .spec
        .as_ref()
        .and_then(|spec| spec.holder_identity.as_deref())
        .filter(|h| !h.is_empty())
        .map(str::to_owned);
    Some(Observed { version, holder })
}

/// When this replica first saw the current version, on its own monotonic clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub version: String,
    pub since: Instant,
}

/// The observation after reading `observed` at `now`: `previous` kept while the version is the
/// same, a new one from `now` when it changed (the Lease was written: it is alive again).
pub fn seen(previous: Option<Seen>, observed: &Observed, now: Instant) -> Seen {
    match previous {
        Some(previous) if previous.version == observed.version => previous,
        _ => Seen {
            version: observed.version.clone(),
            since: now,
        },
    }
}

/// What a standby does after a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standby {
    /// Send the acquire patch.
    Take,
    /// Read again after the retry period.
    Wait,
}

/// Whether a standby takes the Lease: when nobody holds it, when it names `me` (byte for byte), or
/// when its version has not changed for the lease duration. A `seen` of another version than
/// `observed` says nothing about it, so it never makes a held Lease free.
pub fn standby(observed: &Observed, seen: &Seen, me: &str, now: Instant, t: &Timings) -> Standby {
    let free = match observed.holder.as_deref() {
        None => true,
        Some(holder) => holder == me,
    };
    let expired = seen.version == observed.version
        && now.saturating_duration_since(seen.since) >= t.lease_duration;
    if free || expired {
        Standby::Take
    } else {
        Standby::Wait
    }
}

/// What the leader does after a renewal got `409 Conflict` and it read the Lease once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterConflict {
    /// Still named (a Helm label change moved the version): renew once with this version.
    RenewWith(String),
    /// Another holder, or none: step down.
    HeldByAnother,
}

/// The decision after a conflict: renew only while the Lease still names `me`.
pub fn after_conflict(observed: &Observed, me: &str) -> AfterConflict {
    if observed.holder.as_deref() == Some(me) {
        AfterConflict::RenewWith(observed.version.clone())
    } else {
        AfterConflict::HeldByAnother
    }
}

/// When a term ends: the renew deadline after the last good renewal (or the acquire) was *sent*.
pub fn term_end(sent: Instant, t: &Timings) -> Instant {
    sent + t.renew_deadline
}

/// The `leaseTransitions` an acquire writes: the stored count clamped to `0..=i32::MAX` (the Lease
/// is untrusted), plus one, saturating, when the holder changes.
pub fn transitions(stored: Option<i32>, stored_holder: Option<&str>, me: &str) -> i32 {
    let stored = stored.unwrap_or(0).max(0);
    if stored_holder == Some(me) {
        stored
    } else {
        stored.saturating_add(1)
    }
}

/// `now` as a MicroTime: RFC 3339 in UTC with microseconds, as `events.rs` writes times.
fn micro(now: Timestamp) -> Value {
    json!(MicroTime(now))
}

/// The `leaseDurationSeconds` an acquire writes, for `kubectl` readers only.
const WRITTEN_LEASE_SECONDS: u64 = TIMINGS.lease_duration.as_secs();

/// The merge patch that takes the Lease at `version`. Never create-shaped: no name, no labels.
pub fn acquire_body(version: &str, me: &str, now: Timestamp, transitions: i32) -> Value {
    let time = micro(now);
    json!({
        "metadata": {"resourceVersion": version},
        "spec": {
            "holderIdentity": me,
            "leaseDurationSeconds": WRITTEN_LEASE_SECONDS,
            "acquireTime": time,
            "renewTime": time,
            "leaseTransitions": transitions,
        },
    })
}

/// The merge patch that renews the Lease at `version`.
pub fn renew_body(version: &str, me: &str, now: Timestamp) -> Value {
    json!({
        "metadata": {"resourceVersion": version},
        "spec": {"holderIdentity": me, "renewTime": micro(now)},
    })
}

/// The merge patch that releases the Lease at `version` at shutdown: no holder, and a one-second
/// duration for other readers (kausa's own standby never reads it).
pub fn release_body(version: &str, now: Timestamp) -> Value {
    json!({
        "metadata": {"resourceVersion": version},
        "spec": {"holderIdentity": "", "leaseDurationSeconds": 1, "renewTime": micro(now)},
    })
}

#[cfg(test)]
mod tests {
    // Acceptance tests for the standby replica (docs/adr/0019-high-availability.md), written
    // before the code. Every Lease here is untrusted input: only `metadata.resourceVersion` and
    // `spec.holderIdentity` may change an answer. Values are invented.

    use super::*;

    use std::time::{Duration, Instant};

    use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
    use k8s_openapi::jiff::Timestamp;
    use serde_json::{Value, json};

    const ME: &str = "kausa-7d9c8b6f4d-2xkqp";
    const OTHER: &str = "kausa-7d9c8b6f4d-9wzzt";

    /// An origin for the monotonic times of a test: nothing depends on its value.
    fn origin() -> Instant {
        Instant::now()
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn lease(version: Option<&str>, spec: Option<LeaseSpec>) -> Lease {
        Lease {
            metadata: ObjectMeta {
                name: Some("kausa-leader".into()),
                namespace: Some("kausa".into()),
                resource_version: version.map(str::to_owned),
                ..ObjectMeta::default()
            },
            spec,
        }
    }

    fn held_by(version: &str, holder: Option<&str>) -> Lease {
        lease(
            Some(version),
            Some(LeaseSpec {
                holder_identity: holder.map(str::to_owned),
                ..LeaseSpec::default()
            }),
        )
    }

    fn observed(version: &str, holder: Option<&str>) -> Observed {
        observe(&held_by(version, holder)).expect("a Lease with a resourceVersion is observed")
    }

    fn take(answer: Standby) -> bool {
        matches!(answer, Standby::Take)
    }

    // --- Timings ---------------------------------------------------------------------------------

    #[test]
    fn the_timings_are_the_decided_ones() {
        assert_eq!(TIMINGS.lease_duration, secs(30));
        assert_eq!(TIMINGS.renew_deadline, secs(20));
        assert_eq!(TIMINGS.retry_period, secs(5));
        assert_eq!(TIMINGS.call_timeout, secs(5));
        assert_eq!(TIMINGS.release_timeout, secs(2));
        assert_eq!(WRITE_BOUND, secs(10));
        // Also a compile-time assertion in the module: every write a leader starts ends before a
        // standby may take over.
        assert!(TIMINGS.renew_deadline + WRITE_BOUND <= TIMINGS.lease_duration);
    }

    #[test]
    fn a_term_ends_the_renew_deadline_after_the_renewal_was_sent() {
        let sent = origin();
        assert_eq!(term_end(sent, &TIMINGS), sent + secs(20));
        let short = Timings {
            lease_duration: Duration::from_millis(300),
            renew_deadline: Duration::from_millis(200),
            retry_period: Duration::from_millis(50),
            call_timeout: Duration::from_millis(50),
            release_timeout: Duration::from_millis(50),
        };
        assert_eq!(term_end(sent, &short), sent + Duration::from_millis(200));
    }

    // --- observe ---------------------------------------------------------------------------------

    #[test]
    fn observe_keeps_the_version_and_a_non_empty_holder_only() {
        let o = observed("41", Some(OTHER));
        assert_eq!(o.version, "41");
        assert_eq!(o.holder.as_deref(), Some(OTHER));
        // An empty holder is a released Lease.
        assert_eq!(observed("42", Some("")).holder, None);
        // A Lease as the chart creates it (spec: {}), and one without a spec at all.
        assert_eq!(observed("43", None).holder, None);
        let bare = observe(&lease(Some("44"), None)).expect("observed");
        assert_eq!(bare.version, "44");
        assert_eq!(bare.holder, None);
    }

    #[test]
    fn a_lease_without_a_resource_version_is_not_observed() {
        assert!(observe(&lease(None, None)).is_none());
        assert!(
            observe(&lease(
                None,
                Some(LeaseSpec {
                    holder_identity: Some(OTHER.into()),
                    ..LeaseSpec::default()
                })
            ))
            .is_none()
        );
    }

    #[test]
    fn odd_holders_never_panic_and_are_compared_byte_for_byte() {
        let huge = "h".repeat(1024 * 1024);
        for holder in [
            huge.as_str(),
            "\u{fffd}\\xff\\u0000",
            "kausa-7d9c8b6f4d-2xkqp ",
            "KAUSA-7D9C8B6F4D-2XKQP",
            "\u{202e}kausa",
        ] {
            let o = observed("7", Some(holder));
            assert_eq!(o.holder.as_deref(), Some(holder));
            let s = seen(None, &o, origin());
            // None of these is this replica: a standby waits while they hold the Lease.
            assert!(!take(standby(&o, &s, ME, s.since, &TIMINGS)));
        }
    }

    #[test]
    fn an_empty_resource_version_is_not_observed() {
        // An empty precondition would make the next patch unconditional.
        assert!(observe(&held_by("", Some(OTHER))).is_none());
        assert!(observe(&lease(Some(""), None)).is_none());
    }

    // --- seen and standby ------------------------------------------------------------------------

    #[test]
    fn an_observation_of_another_version_never_frees_a_held_lease() {
        let t0 = origin();
        let old = seen(None, &observed("5", Some(OTHER)), t0);
        let newer = observed("6", Some(OTHER));
        assert!(!take(standby(&newer, &old, ME, t0 + secs(3600), &TIMINGS)));
    }

    #[test]
    fn a_free_an_empty_or_an_own_lease_is_taken_at_once() {
        let at = origin();
        for (case, holder) in [("free", None), ("empty", Some("")), ("own", Some(ME))] {
            let o = observed("5", holder);
            let s = seen(None, &o, at);
            assert!(take(standby(&o, &s, ME, at, &TIMINGS)), "{case}");
        }
    }

    #[test]
    fn another_holder_is_taken_only_once_its_version_is_unchanged_for_the_lease_duration() {
        let t0 = origin();
        let o = observed("5", Some(OTHER));
        let s = seen(None, &o, t0);
        assert_eq!(s.version, "5");
        assert_eq!(s.since, t0);
        assert!(!take(standby(&o, &s, ME, t0, &TIMINGS)));
        let just_before = t0 + secs(30) - Duration::from_millis(1);
        assert!(!take(standby(&o, &s, ME, just_before, &TIMINGS)));
        assert!(take(standby(&o, &s, ME, t0 + secs(30), &TIMINGS)));
        assert!(take(standby(&o, &s, ME, t0 + secs(3600), &TIMINGS)));
    }

    #[test]
    fn the_same_version_keeps_when_it_was_first_seen() {
        let t0 = origin();
        let o = observed("5", Some(OTHER));
        let first = seen(None, &o, t0);
        let again = seen(Some(first), &o, t0 + secs(29));
        assert_eq!(again.since, t0);
        assert!(take(standby(&o, &again, ME, t0 + secs(30), &TIMINGS)));
    }

    #[test]
    fn a_new_version_resets_the_observation_time() {
        let t0 = origin();
        let first = seen(None, &observed("5", Some(OTHER)), t0);
        // The leader renewed at 29 s: the Lease is alive again, whoever holds it.
        let renewed = observed("6", Some(OTHER));
        let s = seen(Some(first), &renewed, t0 + secs(29));
        assert_eq!(s.version, "6");
        assert_eq!(s.since, t0 + secs(29));
        assert!(!take(standby(&renewed, &s, ME, t0 + secs(30), &TIMINGS)));
        assert!(!take(standby(
            &renewed,
            &s,
            ME,
            t0 + secs(59) - Duration::from_millis(1),
            &TIMINGS
        )));
        assert!(take(standby(&renewed, &s, ME, t0 + secs(59), &TIMINGS)));
    }

    #[test]
    fn written_durations_and_times_are_never_read() {
        // Leases that differ only in what kausa writes for readers: an edited
        // leaseDurationSeconds or a renewTime far in the past or the future moves no expiry.
        let time = |text: &str| Some(MicroTime(text.parse::<Timestamp>().expect("a time")));
        let variants = [
            LeaseSpec::default(),
            LeaseSpec {
                lease_duration_seconds: Some(1),
                ..LeaseSpec::default()
            },
            LeaseSpec {
                lease_duration_seconds: Some(i32::MAX),
                ..LeaseSpec::default()
            },
            LeaseSpec {
                lease_duration_seconds: Some(-30),
                ..LeaseSpec::default()
            },
            LeaseSpec {
                renew_time: time("1970-01-01T00:00:00Z"),
                acquire_time: time("1970-01-01T00:00:00Z"),
                ..LeaseSpec::default()
            },
            LeaseSpec {
                renew_time: time("2999-12-31T23:59:59Z"),
                acquire_time: time("2999-12-31T23:59:59Z"),
                ..LeaseSpec::default()
            },
        ];
        let t0 = origin();
        for at in [secs(0), secs(29), secs(30), secs(31)] {
            let answers: Vec<bool> = variants
                .iter()
                .map(|spec| {
                    let spec = LeaseSpec {
                        holder_identity: Some(OTHER.into()),
                        ..spec.clone()
                    };
                    let o = observe(&lease(Some("9"), Some(spec))).expect("observed");
                    assert_eq!(o.version, "9");
                    assert_eq!(o.holder.as_deref(), Some(OTHER));
                    let s = seen(None, &o, t0);
                    take(standby(&o, &s, ME, t0 + at, &TIMINGS))
                })
                .collect();
            assert!(
                answers.iter().all(|&a| a == (at >= secs(30))),
                "at {at:?}: {answers:?}"
            );
        }
    }

    // --- after_conflict --------------------------------------------------------------------------

    #[test]
    fn after_a_conflict_a_lease_still_naming_this_pod_is_renewed_with_its_new_version() {
        match after_conflict(&observed("12", Some(ME)), ME) {
            AfterConflict::RenewWith(version) => assert_eq!(version, "12"),
            AfterConflict::HeldByAnother => panic!("still named: renew"),
        }
        for (case, holder) in [("other", Some(OTHER)), ("empty", Some("")), ("free", None)] {
            assert!(
                matches!(
                    after_conflict(&observed("12", holder), ME),
                    AfterConflict::HeldByAnother
                ),
                "{case}"
            );
        }
    }

    // --- transitions -----------------------------------------------------------------------------

    #[test]
    fn transitions_count_holder_changes_clamped_and_saturating() {
        assert_eq!(transitions(None, None, ME), 1);
        assert_eq!(transitions(Some(0), None, ME), 1);
        assert_eq!(transitions(Some(3), Some(OTHER), ME), 4);
        assert_eq!(transitions(Some(3), Some(""), ME), 4);
        // Refused by the API server, but the Lease is untrusted.
        assert_eq!(transitions(Some(-5), Some(OTHER), ME), 1);
        assert_eq!(transitions(Some(i32::MIN), Some(OTHER), ME), 1);
        assert_eq!(transitions(Some(i32::MAX), Some(OTHER), ME), i32::MAX);
        // The same holder again is no transition.
        assert_eq!(transitions(Some(4), Some(ME), ME), 4);
        assert_eq!(transitions(Some(-5), Some(ME), ME), 0);
        assert_eq!(transitions(None, Some(ME), ME), 0);
    }

    // --- Bodies ----------------------------------------------------------------------------------

    /// A time with nanoseconds: the bodies carry it as a MicroTime, cut to microseconds.
    fn now() -> Timestamp {
        "2026-10-29T12:34:56.123456789Z".parse().expect("a time")
    }

    const MICRO: &str = "2026-10-29T12:34:56.123456Z";

    fn assert_not_create_shaped(body: &Value) {
        let metadata = body["metadata"].as_object().expect("metadata");
        assert_eq!(
            metadata.keys().collect::<Vec<_>>(),
            ["resourceVersion"],
            "only the precondition"
        );
        assert_eq!(
            body.as_object().map(|o| o.len()),
            Some(2),
            "metadata and spec only"
        );
    }

    #[test]
    fn the_acquire_body_is_exact() {
        let body = acquire_body("41", ME, now(), 4);
        assert_eq!(
            body,
            json!({
                "metadata": {"resourceVersion": "41"},
                "spec": {
                    "holderIdentity": ME,
                    "leaseDurationSeconds": 30,
                    "acquireTime": MICRO,
                    "renewTime": MICRO,
                    "leaseTransitions": 4,
                },
            })
        );
        assert_not_create_shaped(&body);
    }

    #[test]
    fn the_renew_body_is_exact() {
        let body = renew_body("42", ME, now());
        assert_eq!(
            body,
            json!({
                "metadata": {"resourceVersion": "42"},
                "spec": {"holderIdentity": ME, "renewTime": MICRO},
            })
        );
        assert_not_create_shaped(&body);
    }

    #[test]
    fn the_release_body_is_exact() {
        let body = release_body("43", now());
        assert_eq!(
            body,
            json!({
                "metadata": {"resourceVersion": "43"},
                "spec": {"holderIdentity": "", "leaseDurationSeconds": 1, "renewTime": MICRO},
            })
        );
        assert_not_create_shaped(&body);
    }

    #[test]
    fn bodies_carry_odd_values_as_json_strings() {
        // The version is echoed as read; it can never break the body's structure.
        let version = "1\"},\"metadata\":{\"name\":\"x";
        for body in [
            acquire_body(version, ME, now(), 1),
            renew_body(version, ME, now()),
            release_body(version, now()),
        ] {
            assert_eq!(body["metadata"]["resourceVersion"], json!(version));
            assert_not_create_shaped(&body);
        }
    }
}
