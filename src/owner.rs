//! Owner correlation: from a Pod (or a Job) to the top-level owner that an incident is about.
//!
//! A Pod's controller owner reference already names its ReplicaSet, StatefulSet, DaemonSet or Job.
//! Only two hops need a read, ReplicaSet → Deployment and Job → CronJob, and they read metadata
//! only; kausa follows no other kind, so a custom resource that owns a ReplicaSet or a Pod is the
//! top-level owner, named from the reference alone (docs/adr/0009-least-privilege-rbac.md).

use std::future::Future;

use crate::record::OwnerRef;

/// The kinds whose controller owner kausa reads, and the only ones it has permission to `get`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hop {
    ReplicaSet,
    Job,
}

impl Hop {
    /// The hop for an owner reference, if kausa follows it: only `apps/v1` ReplicaSets and
    /// `batch/v1` Jobs.
    pub fn of(owner: &OwnerRef) -> Option<Hop> {
        match (owner.api_version.as_str(), owner.kind.as_str()) {
            ("apps/v1", "ReplicaSet") => Some(Hop::ReplicaSet),
            ("batch/v1", "Job") => Some(Hop::Job),
            _ => None,
        }
    }
}

/// The longest name Kubernetes accepts for the objects kausa looks up (a DNS-1123 subdomain).
const MAX_NAME_LEN: usize = 253;

/// The longest namespace Kubernetes accepts (a DNS-1123 label).
const MAX_NAMESPACE_LEN: usize = 63;

/// What a lookup answers for a name that is not a DNS-1123 subdomain or a namespace that is not a
/// DNS-1123 label: it is never sent, so the answer is a failed lookup that quotes nothing from the
/// cluster.
pub(crate) const REFUSED: &str = "owner name or namespace is not a valid DNS name; not looked up";

/// Whether `name` is a DNS-1123 subdomain as Kubernetes defines one (`IsDNS1123Subdomain`): 1 to
/// 253 bytes, dot-separated labels of lowercase letters, digits and `-`, each starting and ending
/// with a letter or a digit. Such a name has no `/`, no `?`, no `..` segment and no control
/// character, so it is safe in a URL path.
pub(crate) fn is_dns_subdomain(name: &str) -> bool {
    name.len() <= MAX_NAME_LEN
        && name.split('.').all(|label| {
            let bytes = label.as_bytes();
            let edge = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
            !bytes.is_empty()
                && bytes.first().is_some_and(edge)
                && bytes.last().is_some_and(edge)
                && bytes.iter().all(|b| edge(b) || *b == b'-')
        })
}

/// Whether `namespace` is a DNS-1123 label as Kubernetes defines one: 1 to 63 bytes of lowercase
/// letters, digits and `-`, starting and ending with a letter or a digit. It has no `/`, no `.`
/// and no control character, so it is safe in a URL path.
pub(crate) fn is_dns_label(namespace: &str) -> bool {
    let bytes = namespace.as_bytes();
    let edge = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    (1..=MAX_NAMESPACE_LEN).contains(&bytes.len())
        && bytes.first().is_some_and(edge)
        && bytes.last().is_some_and(edge)
        && bytes.iter().all(|b| edge(b) || *b == b'-')
}

/// `name` cut at 253 characters, for a log line: the name of an Event's object is unbounded, and
/// the log serializer escapes it but does not shorten it. A name that passes `is_dns_subdomain`
/// is unchanged.
pub(crate) fn bounded_name(name: &str) -> String {
    name.chars().take(MAX_NAME_LEN).collect()
}

/// Reads the controller owner of a ReplicaSet or a Job. The name comes from an `ownerReference`
/// or an Event's `regarding`, which any writer of those objects chooses; an implementation that
/// sends it to the API server answers `Err(REFUSED)`, with no request and no cache entry, when
/// `is_dns_subdomain` is false for it or `is_dns_label` is false for the namespace.
pub(crate) trait OwnerLookup {
    /// The controller owner reference of `name` in `namespace`, `Ok(None)` when it has none.
    fn controller_of(
        &self,
        hop: Hop,
        namespace: &str,
        name: &str,
    ) -> impl Future<Output = Result<Option<OwnerRef>, String>>;

    /// The lookups held in a cache, expired ones included (the `kausa_owner_cache_entries`
    /// gauge); 0 for a lookup without one.
    fn cached(&self) -> usize {
        0
    }
}

/// The top-level owner of an object whose controller owner is `first`, or `fallback` (the object
/// itself) when it has none. A lookup that fails stops the chain where it is, so a temporary API
/// error makes the incident name the ReplicaSet or Job instead of losing it; the error is returned
/// for the log.
pub(crate) async fn top_owner<L: OwnerLookup>(
    lookup: &L,
    namespace: &str,
    first: Option<&OwnerRef>,
    fallback: &OwnerRef,
) -> (OwnerRef, Option<String>) {
    let Some(first) = first else {
        return (fallback.clone(), None);
    };
    let Some(hop) = Hop::of(first) else {
        return (first.clone(), None);
    };
    match lookup.controller_of(hop, namespace, &first.name).await {
        Ok(Some(parent)) => (parent, None),
        Ok(None) => (first.clone(), None),
        Err(error) => (first.clone(), Some(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn owner(api_version: &str, kind: &str, name: &str) -> OwnerRef {
        OwnerRef {
            api_version: api_version.into(),
            kind: kind.into(),
            name: name.into(),
            uid: format!("uid-{name}"),
        }
    }

    struct Fake(HashMap<(String, String), Result<Option<OwnerRef>, String>>);

    impl OwnerLookup for Fake {
        async fn controller_of(
            &self,
            hop: Hop,
            namespace: &str,
            name: &str,
        ) -> Result<Option<OwnerRef>, String> {
            let kind = match hop {
                Hop::ReplicaSet => "rs",
                Hop::Job => "job",
            };
            self.0
                .get(&(format!("{namespace}/{kind}"), name.to_owned()))
                .cloned()
                .unwrap_or(Ok(None))
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    #[test]
    fn a_lease_name_is_a_dns_subdomain() {
        let longest = format!("{}.{}", "a".repeat(126), "b".repeat(126));
        for name in [
            "a",
            "0",
            "kausa-leader",
            "kausa.leader",
            "a-b.c-d.e",
            &longest,
        ] {
            assert!(is_dns_subdomain(name), "{name}");
        }
        let too_long = format!("{longest}c");
        for name in [
            "", ".", "a..b", "a-.b", "a.-b", "A", "a_b", "a/b", &too_long,
        ] {
            assert!(!is_dns_subdomain(name), "{name:?}");
        }
    }

    #[test]
    fn only_a_dns_subdomain_is_a_name_to_look_up() {
        let longest = format!("{0}.{0}.{0}.{1}", "a".repeat(63), "a".repeat(61));
        assert_eq!(longest.len(), 253);
        for valid in [
            "a",
            "0",
            "a-b",
            "web-7d9c8b6f4d",
            "a.b",
            "a".repeat(253).as_str(),
            &longest,
        ] {
            assert!(is_dns_subdomain(valid), "{valid}");
        }
        let too_long = "a".repeat(254);
        for refused in [
            "",
            "a/b",
            "../x",
            "a..b",
            "a.",
            ".a",
            "a?x=1",
            "a#b",
            "a%2fb",
            "A",
            "web_1",
            "-a",
            "a-",
            "a b",
            "a\nb",
            "a\0b",
            "é",
            too_long.as_str(),
        ] {
            assert!(!is_dns_subdomain(refused), "{refused:?}");
        }
    }

    #[test]
    fn only_a_dns_label_is_a_namespace_to_look_up() {
        for valid in ["a", "0", "a-b", "shop", "a".repeat(63).as_str()] {
            assert!(is_dns_label(valid), "{valid}");
        }
        let too_long = "a".repeat(64);
        for refused in [
            "",
            "a/b",
            "..",
            ".",
            "a.b",
            "a?x=1",
            "Shop",
            "-a",
            "a-",
            "a_b",
            "a\nb",
            "é",
            too_long.as_str(),
        ] {
            assert!(!is_dns_label(refused), "{refused:?}");
        }
    }

    #[test]
    fn a_name_in_a_log_line_is_cut_at_253_characters() {
        assert_eq!(bounded_name("web"), "web");
        assert_eq!(bounded_name(&"a".repeat(253)).len(), 253);
        assert_eq!(bounded_name(&"é".repeat(300)).chars().count(), 253);
    }

    #[test]
    fn a_replicaset_leads_to_its_deployment_and_a_job_to_its_cronjob() {
        let fake = Fake(HashMap::from([
            (
                ("shop/rs".into(), "api-7d9c8b6f4d".into()),
                Ok(Some(owner("apps/v1", "Deployment", "api"))),
            ),
            (
                ("shop/job".into(), "report-29000000".into()),
                Ok(Some(owner("batch/v1", "CronJob", "report"))),
            ),
        ]));
        let pod = owner("v1", "Pod", "api-7d9c8b6f4d-2xkqp");
        let rs = owner("apps/v1", "ReplicaSet", "api-7d9c8b6f4d");
        let (top, error) = block_on(top_owner(&fake, "shop", Some(&rs), &pod));
        assert_eq!(
            (top.kind.as_str(), top.name.as_str()),
            ("Deployment", "api")
        );
        assert_eq!(error, None);
        let job = owner("batch/v1", "Job", "report-29000000");
        let (top, _) = block_on(top_owner(&fake, "shop", Some(&job), &pod));
        assert_eq!(
            (top.kind.as_str(), top.name.as_str()),
            ("CronJob", "report")
        );
    }

    #[test]
    fn other_kinds_and_orphans_are_the_top_owner() {
        let fake = Fake(HashMap::new());
        let pod = owner("v1", "Pod", "db-0");
        let sts = owner("apps/v1", "StatefulSet", "db");
        let (top, _) = block_on(top_owner(&fake, "shop", Some(&sts), &pod));
        assert_eq!(top.kind, "StatefulSet");
        // A custom resource that owns a Pod is named from the reference, never read.
        let custom = owner("example.com/v1", "Widget", "w");
        let (top, _) = block_on(top_owner(&fake, "shop", Some(&custom), &pod));
        assert_eq!(top.kind, "Widget");
        let (top, _) = block_on(top_owner(&fake, "shop", None, &pod));
        assert_eq!(top.kind, "Pod");
        // A ReplicaSet with no owner (created by hand) is its own top-level owner.
        let rs = owner("apps/v1", "ReplicaSet", "lonely");
        let (top, _) = block_on(top_owner(&fake, "shop", Some(&rs), &pod));
        assert_eq!(top.kind, "ReplicaSet");
    }

    #[test]
    fn a_cronjob_is_its_own_top_owner_with_no_read() {
        // A CronJob's own event (`FailedCreate` when the API server refuses its Job, D011) is about
        // the CronJob: no hop, so no read and no new permission. The lookup would name another
        // owner if kausa asked it.
        let cron = owner("batch/v1", "CronJob", "report");
        assert_eq!(Hop::of(&cron), None);
        let fake = Fake(HashMap::from([(
            ("shop/job".into(), "report".into()),
            Ok(Some(owner("example.com/v1", "Widget", "w"))),
        )]));
        let (top, error) = block_on(top_owner(&fake, "shop", Some(&cron), &cron));
        assert_eq!(top, cron);
        assert_eq!(error, None);
    }

    #[test]
    fn a_failed_lookup_stops_at_the_replicaset_and_reports_the_error() {
        let fake = Fake(HashMap::from([(
            ("shop/rs".into(), "api-1".into()),
            Err("forbidden".into()),
        )]));
        let pod = owner("v1", "Pod", "api-1-x");
        let rs = owner("apps/v1", "ReplicaSet", "api-1");
        let (top, error) = block_on(top_owner(&fake, "shop", Some(&rs), &pod));
        assert_eq!(top.kind, "ReplicaSet");
        assert_eq!(error.as_deref(), Some("forbidden"));
    }
}
