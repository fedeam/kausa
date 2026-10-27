//! Cost controls (docs/explanation/architecture.md, "Cost controls"): a cap on model calls per
//! hour and per day, and a cache of explanations by incident fingerprint, so that a repeated
//! incident is explained again without a new call.
//!
//! Both live in memory and start over when kausa restarts; the hard limit on cost is the one set
//! on the provider's key (SECURITY.md). They measure time with the monotonic clock, so a change of
//! the wall clock neither frees nor freezes the budget.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::prompt::Explanation;

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);

/// How many model calls may start in the last hour and in the last day. `0` means no call at all.
#[derive(Debug)]
pub struct Budget {
    per_hour: usize,
    per_day: usize,
    /// The start times of the calls of the last day, oldest first.
    calls: VecDeque<Instant>,
}

impl Budget {
    pub fn new(per_hour: usize, per_day: usize) -> Self {
        Self {
            per_hour,
            per_day,
            calls: VecDeque::new(),
        }
    }

    /// Takes one call from the budget at `now`, or says which limit is spent.
    pub fn take(&mut self, now: Instant) -> Result<(), &'static str> {
        while self
            .calls
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= DAY)
        {
            self.calls.pop_front();
        }
        let last_hour = self
            .calls
            .iter()
            .filter(|&&t| now.saturating_duration_since(t) < HOUR)
            .count();
        if last_hour >= self.per_hour {
            return Err("the hourly call budget is spent");
        }
        if self.calls.len() >= self.per_day {
            return Err("the daily call budget is spent");
        }
        self.calls.push_back(now);
        Ok(())
    }

    /// Gives back the call taken last, for a request that never reached the endpoint.
    pub fn refund(&mut self) {
        self.calls.pop_back();
    }
}

/// A stored explanation and when it was stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cached {
    pub explanation: Explanation,
    pub stored_at: Instant,
}

/// Explanations by incident fingerprint, kept for `ttl`, at most `capacity` of them (the oldest
/// goes first).
#[derive(Debug)]
pub struct Dedup {
    ttl: Duration,
    capacity: usize,
    entries: HashMap<String, Cached>,
}

impl Dedup {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            entries: HashMap::new(),
        }
    }

    /// The explanation stored for `fingerprint`, if it has not expired at `now`.
    pub fn get(&mut self, fingerprint: &str, now: Instant) -> Option<Explanation> {
        let fresh = self
            .entries
            .get(fingerprint)
            .is_some_and(|c| now.saturating_duration_since(c.stored_at) < self.ttl);
        if !fresh {
            self.entries.remove(fingerprint);
            return None;
        }
        self.entries.get(fingerprint).map(|c| c.explanation.clone())
    }

    pub fn put(&mut self, fingerprint: &str, explanation: Explanation, now: Instant) {
        if self.capacity == 0 {
            return;
        }
        if !self.entries.contains_key(fingerprint) && self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by(|a, b| a.1.stored_at.cmp(&b.1.stored_at).then(a.0.cmp(b.0)))
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            fingerprint.to_owned(),
            Cached {
                explanation,
                stored_at: now,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(seconds: u64) -> Instant {
        static BASE: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
        *BASE + Duration::from_secs(seconds)
    }

    fn explanation(summary: &str) -> Explanation {
        Explanation {
            summary: summary.into(),
            cause: "c".into(),
            steps: vec![],
            confidence: "low".into(),
        }
    }

    #[test]
    fn the_hourly_cap_holds_and_frees_up_after_an_hour() {
        let mut budget = Budget::new(2, 100);
        assert!(budget.take(t(0)).is_ok());
        assert!(budget.take(t(10)).is_ok());
        assert_eq!(budget.take(t(20)), Err("the hourly call budget is spent"));
        assert!(budget.take(t(3600)).is_ok()); // the first call is an hour old
    }

    #[test]
    fn the_daily_cap_holds() {
        let mut budget = Budget::new(10, 3);
        for i in 0..3 {
            assert!(budget.take(t(i * 4000)).is_ok());
        }
        assert_eq!(
            budget.take(t(13_000)),
            Err("the daily call budget is spent")
        );
        assert!(budget.take(t(86_400)).is_ok());
    }

    #[test]
    fn an_earlier_instant_frees_nothing() {
        let mut budget = Budget::new(1, 10);
        assert!(budget.take(t(100)).is_ok());
        assert!(budget.take(t(50)).is_err());
    }

    #[test]
    fn a_refunded_call_frees_its_place() {
        let mut budget = Budget::new(1, 10);
        assert!(budget.take(t(0)).is_ok());
        budget.refund();
        assert!(budget.take(t(1)).is_ok());
        assert!(budget.take(t(2)).is_err());
    }

    #[test]
    fn a_zero_budget_allows_nothing() {
        assert!(Budget::new(0, 0).take(t(0)).is_err());
    }

    #[test]
    fn explanations_are_reused_until_they_expire() {
        let mut dedup = Dedup::new(Duration::from_secs(6 * 3600), 10);
        dedup.put("f1", explanation("a"), t(0));
        assert_eq!(dedup.get("f1", t(3600)), Some(explanation("a")));
        assert_eq!(dedup.get("f1", t(6 * 3600)), None);
        assert_eq!(dedup.get("f2", t(0)), None);
    }

    #[test]
    fn the_cache_keeps_its_capacity_by_dropping_the_oldest() {
        let mut dedup = Dedup::new(Duration::from_secs(6 * 3600), 2);
        dedup.put("a", explanation("a"), t(0));
        dedup.put("b", explanation("b"), t(1));
        dedup.put("c", explanation("c"), t(2));
        assert_eq!(dedup.get("a", t(3)), None);
        assert!(dedup.get("b", t(3)).is_some());
        assert!(dedup.get("c", t(3)).is_some());
    }
}
