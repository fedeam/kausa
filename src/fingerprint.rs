//! The incident fingerprint: a hash of what an incident is, stable across restarts of the same
//! failure, so that a repeated incident can reuse its explanation (deduplication, third weekend).
//!
//! It is computed in memory before redaction and is never sent or written anywhere but kausa's
//! own log (docs/reference/rules.md, "Grouping and fingerprint").

/// The prefix client-go's event recorder puts before the note of an event that aggregates similar
/// ones (`EventAggregatorByReasonMessageFunc` in `tools/record/events_cache.go`, checked on
/// 2026-10-28).
const COMBINED_PREFIX: &str = "(combined from similar events): ";

/// Normalizes a message so that the same failure gives the same text: the prefix of an aggregated
/// event, `(combined from similar events): `, is dropped, the names of the Pods involved become
/// `<pod>`, UUIDs `<uuid>`, Go durations such as `40s` or `2m40s` (the kubelet's growing back-off)
/// `<duration>`, hexadecimal runs of eight or more characters `<hex>` and every other run of
/// digits `#`.
pub fn normalize(message: &str, pod_names: &[&str]) -> String {
    let mut text = message
        .strip_prefix(COMBINED_PREFIX)
        .unwrap_or(message)
        .to_owned();
    // Longest names first, so that a name that contains another is replaced whole.
    let mut names: Vec<&str> = pod_names
        .iter()
        .copied()
        .filter(|n| !n.is_empty())
        .collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for name in names {
        text = text.replace(name, "<pod>");
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    let mut after_word = false;
    while let Some(c) = rest.chars().next() {
        if let Some(len) = (!after_word).then(|| duration_len(rest)).flatten() {
            out.push_str("<duration>");
            rest = &rest[len..];
            after_word = true;
            continue;
        }
        after_word = c.is_alphanumeric();
        if let Some(len) = uuid_len(rest) {
            out.push_str("<uuid>");
            rest = &rest[len..];
        } else if let Some(len) = hex_run_len(rest) {
            out.push_str("<hex>");
            rest = &rest[len..];
        } else if c.is_ascii_digit() {
            out.push('#');
            rest = rest.trim_start_matches(|d: char| d.is_ascii_digit());
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The fingerprint of an incident, as 16 lowercase hexadecimal digits: FNV-1a (64 bits) over the
/// detection id, the namespace, the owner's kind and name, and the normalized message, separated
/// by a byte that none of them contains. Not a cryptographic hash: it only has to be stable.
pub fn fingerprint(
    detection_id: &str,
    namespace: &str,
    owner_kind: &str,
    owner_name: &str,
    normalized_message: &str,
) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let parts = [
        detection_id,
        namespace,
        owner_kind,
        owner_name,
        normalized_message,
    ];
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hash ^= 0x1f; // the ASCII unit separator
            hash = hash.wrapping_mul(PRIME);
        }
        for byte in part.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    format!("{hash:016x}")
}

/// The length of a Go duration (`300ms`, `40s`, `2m40s`, `1h0m0s`, `2.5s`) at the start of `s`:
/// one or more groups of digits, an optional fraction and a unit, not followed by a letter or a
/// digit.
fn duration_len(s: &str) -> Option<usize> {
    const UNITS: [&str; 7] = ["ms", "us", "µs", "ns", "h", "m", "s"];
    let mut len = 0;
    loop {
        let rest = &s[len..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            break;
        }
        let mut end = digits;
        if rest[end..].starts_with('.') {
            let fraction = rest[end + 1..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            if fraction > 0 {
                end += 1 + fraction;
            }
        }
        let Some(unit) = UNITS.iter().find(|u| rest[end..].starts_with(**u)) else {
            break;
        };
        len += end + unit.len();
    }
    let next = s[len..].chars().next();
    (len > 0 && !next.is_some_and(char::is_alphanumeric)).then_some(len)
}

/// The length of a UUID (8-4-4-4-12 hexadecimal digits) at the start of `s`, if there is one that
/// is not part of a longer hexadecimal word.
fn uuid_len(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.len() < 36 {
        return None;
    }
    for (i, &b) in bytes[..36].iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        };
        if !ok {
            return None;
        }
    }
    match bytes.get(36) {
        Some(b) if b.is_ascii_hexdigit() => None,
        _ => Some(36),
    }
}

/// The length of a run of eight or more hexadecimal digits at the start of `s` that contains at
/// least one digit and one letter (so that plain numbers and plain words are not hex runs).
fn hex_run_len(s: &str) -> Option<usize> {
    let len = s.bytes().take_while(u8::is_ascii_hexdigit).count();
    let run = &s.as_bytes()[..len];
    let has_digit = run.iter().any(u8::is_ascii_digit);
    let has_letter = run.iter().any(u8::is_ascii_alphabetic);
    (len >= 8 && has_digit && has_letter).then_some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_failure_on_another_pod_normalizes_the_same() {
        let a = normalize(
            "Back-off 40s restarting failed container api in pod api-7d9c8b6f4d-2xkqp",
            &["api-7d9c8b6f4d-2xkqp"],
        );
        let b = normalize(
            "Back-off 2m40s restarting failed container api in pod api-7d9c8b6f4d-8hn2w",
            &["api-7d9c8b6f4d-8hn2w"],
        );
        assert_eq!(
            a,
            "Back-off <duration> restarting failed container api in pod <pod>"
        );
        assert_eq!(a, b);
    }

    #[test]
    fn uuids_hex_runs_and_digits() {
        assert_eq!(
            normalize(
                "volume 11111111-2222-4333-8444-555555555555 sha 9f12ed4c49936e09 port 5432",
                &[]
            ),
            "volume <uuid> sha <hex> port #"
        );
        // Plain words made of hex letters stay, and so does non-ASCII text.
        assert_eq!(normalize("deadbeef café", &[]), "deadbeef café");
    }

    #[test]
    fn durations() {
        assert_eq!(
            normalize("back-off 5m0s, timeout 2.5s after 300ms", &[]),
            "back-off <duration>, timeout <duration> after <duration>"
        );
        // Not durations: a unit glued to more letters, a number inside a word, a bare number.
        assert_eq!(normalize("10mb v2s 42", &[]), "#mb v#s #");
    }

    #[test]
    fn longer_pod_names_are_replaced_first() {
        assert_eq!(
            normalize("job-a and job-a-2", &["job-a", "job-a-2"]),
            "<pod> and <pod>"
        );
    }

    #[test]
    fn a_combined_event_has_the_fingerprint_of_a_single_one() {
        // The kubelet writes each failure to create a sandbox as its own event, then folds them
        // into one aggregated event; both are the same incident.
        let note = |sandbox: &str| {
            format!(
                "Failed to create pod sandbox: rpc error: code = Unknown desc = unable to get OCI \
                 runtime for sandbox \"{sandbox}\": no runtime for \"gvisor\" is configured"
            )
        };
        let single = note(&"a1".repeat(32));
        let combined = format!("(combined from similar events): {}", note(&"9f".repeat(32)));
        let print = |message: &str| {
            fingerprint(
                "D010",
                "shop",
                "Deployment",
                "api",
                &normalize(message, &[]),
            )
        };
        assert_eq!(normalize(&single, &[]), normalize(&combined, &[]));
        assert_eq!(print(&single), print(&combined));
        // Only a leading prefix is dropped.
        assert_eq!(
            normalize("x (combined from similar events): y", &[]),
            "x (combined from similar events): y"
        );
    }

    #[test]
    fn the_fingerprint_is_stable_and_separates_its_parts() {
        let f = fingerprint("D001", "shop", "Deployment", "api", "x");
        assert_eq!(f.len(), 16);
        assert_eq!(f, fingerprint("D001", "shop", "Deployment", "api", "x"));
        assert_ne!(f, fingerprint("D001", "shop", "Deployment", "apix", ""));
        assert_ne!(f, fingerprint("D002", "shop", "Deployment", "api", "x"));
    }

    #[test]
    fn the_hash_is_fnv_1a() {
        // FNV-1a 64 of "a" is af63dc4c8601ec8c (the published test vector); with the four empty
        // parts after it, four separators follow it.
        let mut hash: u64 = 0xaf63_dc4c_8601_ec8c;
        for _ in 0..4 {
            hash ^= 0x1f;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        assert_eq!(fingerprint("a", "", "", "", ""), format!("{hash:016x}"));
    }
}
