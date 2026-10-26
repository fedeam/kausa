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

/// Reads the controller owner of a ReplicaSet or a Job.
pub(crate) trait OwnerLookup {
    /// The controller owner reference of `name` in `namespace`, `Ok(None)` when it has none.
    fn controller_of(
        &self,
        hop: Hop,
        namespace: &str,
        name: &str,
    ) -> impl Future<Output = Result<Option<OwnerRef>, String>>;
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
