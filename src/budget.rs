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

/// A stored explanation and when it was stored. With notifications on
/// (docs/adr/0015-notifications.md), the entry also keeps the notification's version of the
/// explanation, which restores only the namespace and the top-level owner, and whether that
/// version was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cached {
    pub explanation: Explanation,
    /// The notification's text, `None` with notifications off.
    pub notice: Option<Explanation>,
    /// Whether the webhook acknowledged `notice` with a `2xx`.
    pub notified: bool,
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

    /// The entry stored for `fingerprint`, if it has not expired at `now`; an expired entry is
    /// removed.
    pub fn get(&mut self, fingerprint: &str, now: Instant) -> Option<Cached> {
        let fresh = self
            .entries
            .get(fingerprint)
            .is_some_and(|c| now.saturating_duration_since(c.stored_at) < self.ttl);
        if !fresh {
            self.entries.remove(fingerprint);
            return None;
        }
        self.entries.get(fingerprint).cloned()
    }

    /// Stores `explanation` and, with notifications on, its `notice`. The entry starts not
    /// notified, also when it replaces one: a new model call wrote it.
    pub fn put(
        &mut self,
        fingerprint: &str,
        explanation: Explanation,
        notice: Option<Explanation>,
        now: Instant,
    ) {
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
                notice,
                notified: false,
                stored_at: now,
            },
        );
    }

    /// The entry stored for `fingerprint`, expired or not; removes nothing.
    pub fn peek(&self, fingerprint: &str) -> Option<&Cached> {
        self.entries.get(fingerprint)
    }

    /// Marks the entry of `fingerprint` notified, only if it still stores `notice`: an evicted
    /// entry, or one replaced by another text, is left as it is. Returns whether it is marked.
    pub fn mark_notified(&mut self, fingerprint: &str, notice: &Explanation) -> bool {
        match self.entries.get_mut(fingerprint) {
            Some(cached) if cached.notice.as_ref() == Some(notice) => {
                cached.notified = true;
                true
            }
            _ => false,
        }
    }

    /// Whether `notice` was already sent for `fingerprint`: the entry, expired or not, stores it
    /// and is notified.
    pub fn already_sent(&self, fingerprint: &str, notice: &Explanation) -> bool {
        self.peek(fingerprint)
            .is_some_and(|c| c.notified && c.notice.as_ref() == Some(notice))
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
        dedup.put("f1", explanation("a"), None, t(0));
        assert_eq!(
            dedup.get("f1", t(3600)).map(|c| c.explanation),
            Some(explanation("a"))
        );
        assert_eq!(dedup.get("f1", t(6 * 3600)), None);
        assert_eq!(dedup.get("f2", t(0)), None);
    }

    #[test]
    fn the_cache_keeps_its_capacity_by_dropping_the_oldest() {
        let mut dedup = Dedup::new(Duration::from_secs(6 * 3600), 2);
        dedup.put("a", explanation("a"), None, t(0));
        dedup.put("b", explanation("b"), None, t(1));
        dedup.put("c", explanation("c"), None, t(2));
        assert_eq!(dedup.get("a", t(3)), None);
        assert!(dedup.get("b", t(3)).is_some());
        assert!(dedup.get("c", t(3)).is_some());
    }

    // Acceptance tests for notifications (docs/adr/0015-notifications.md), written before the
    // code. They use this API, which budget.rs does not have yet:
    //
    // - `Cached { explanation, notice: Option<Explanation>, notified: bool, stored_at }`, every
    //   field `pub`;
    // - `Dedup::put(&mut self, fingerprint, explanation, notice: Option<Explanation>, now)`, which
    //   always stores `notified: false`, also when it replaces an entry;
    // - `Dedup::get(&mut self, fingerprint, now) -> Option<Cached>`: the TTL and the removal of
    //   today, returning a clone of the entry;
    // - `Dedup::peek(&self, fingerprint) -> Option<&Cached>`: no TTL, removes nothing;
    // - `Dedup::mark_notified(&mut self, fingerprint, notice: &Explanation) -> bool`: sets
    //   `notified` only when the entry exists and stores `Some(notice)`;
    // - `Dedup::already_sent(&self, fingerprint, notice: &Explanation) -> bool`: the entry stores
    //   `Some(notice)` and `notified` is true.

    const TTL: Duration = Duration::from_secs(6 * 3600);

    #[test]
    fn a_stored_entry_keeps_its_notice_and_starts_not_notified() {
        let mut dedup = Dedup::new(TTL, 10);
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(0));
        let cached = dedup.peek("f").expect("an entry");
        assert_eq!(cached.explanation, explanation("full"));
        assert_eq!(cached.notice, Some(explanation("narrow")));
        assert!(!cached.notified);
        assert_eq!(cached.stored_at, t(0));
        let got = dedup.get("f", t(1)).expect("a fresh entry");
        assert_eq!(got.explanation, explanation("full"));
        assert_eq!(got.notice, Some(explanation("narrow")));
        assert!(!got.notified);
        // Without notifications there is no notice.
        dedup.put("g", explanation("full"), None, t(0));
        assert_eq!(dedup.peek("g").expect("an entry").notice, None);
    }

    #[test]
    fn a_successful_send_marks_the_entry_that_stores_its_notice() {
        let mut dedup = Dedup::new(TTL, 10);
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(0));
        assert!(!dedup.already_sent("f", &explanation("narrow")));
        assert!(dedup.mark_notified("f", &explanation("narrow")));
        assert!(dedup.peek("f").expect("an entry").notified);
        assert!(dedup.already_sent("f", &explanation("narrow")));
        assert!(dedup.get("f", t(1)).expect("a fresh entry").notified);
        // Marking twice changes nothing.
        assert!(dedup.mark_notified("f", &explanation("narrow")));
        assert!(dedup.peek("f").expect("an entry").notified);
    }

    #[test]
    fn mark_notified_marks_nothing_for_another_text_or_a_missing_entry() {
        let mut dedup = Dedup::new(TTL, 10);
        assert!(!dedup.mark_notified("missing", &explanation("narrow")));
        assert!(dedup.peek("missing").is_none());
        // An entry stored without a notice is never marked.
        dedup.put("plain", explanation("full"), None, t(0));
        assert!(!dedup.mark_notified("plain", &explanation("full")));
        assert!(!dedup.peek("plain").expect("an entry").notified);
        // An entry replaced by another text is not marked by the older send.
        dedup.put("f", explanation("full"), Some(explanation("old")), t(0));
        dedup.put("f", explanation("full 2"), Some(explanation("new")), t(10));
        assert!(!dedup.mark_notified("f", &explanation("old")));
        assert!(!dedup.peek("f").expect("an entry").notified);
        assert!(!dedup.already_sent("f", &explanation("old")));
        assert!(!dedup.already_sent("f", &explanation("new")));
        assert!(dedup.mark_notified("f", &explanation("new")));
        assert!(!dedup.already_sent("f", &explanation("old")));
        assert!(dedup.already_sent("f", &explanation("new")));
    }

    #[test]
    fn a_replacing_put_starts_not_notified_again() {
        let mut dedup = Dedup::new(TTL, 10);
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(0));
        assert!(dedup.mark_notified("f", &explanation("narrow")));
        // The same text stored again (a new model call after the entry was used) is a new entry.
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(5));
        let cached = dedup.peek("f").expect("an entry");
        assert!(!cached.notified);
        assert_eq!(cached.stored_at, t(5));
        assert!(!dedup.already_sent("f", &explanation("narrow")));
    }

    #[test]
    fn the_notified_mark_goes_with_its_entry() {
        // Expiry: get removes the entry, and peek then finds nothing.
        let mut dedup = Dedup::new(TTL, 10);
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(0));
        assert!(dedup.mark_notified("f", &explanation("narrow")));
        assert_eq!(dedup.get("f", t(6 * 3600)), None);
        assert!(dedup.peek("f").is_none());
        assert!(!dedup.already_sent("f", &explanation("narrow")));
        // Eviction: the oldest entry goes, with its mark.
        let mut dedup = Dedup::new(TTL, 1);
        dedup.put("a", explanation("a"), Some(explanation("na")), t(0));
        assert!(dedup.mark_notified("a", &explanation("na")));
        dedup.put("b", explanation("b"), Some(explanation("nb")), t(1));
        assert!(dedup.peek("a").is_none());
        assert!(!dedup.already_sent("a", &explanation("na")));
        assert!(!dedup.mark_notified("a", &explanation("na")));
        assert!(!dedup.peek("b").expect("an entry").notified);
    }

    #[test]
    fn peek_applies_no_ttl_and_removes_nothing() {
        let mut dedup = Dedup::new(TTL, 10);
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(0));
        assert!(dedup.mark_notified("f", &explanation("narrow")));
        // Long after the TTL, peek still sees the entry, as often as asked.
        for _ in 0..3 {
            let cached = dedup.peek("f").expect("an entry");
            assert!(cached.notified);
            assert_eq!(cached.notice, Some(explanation("narrow")));
        }
        assert!(dedup.already_sent("f", &explanation("narrow")));
        // get still expires it.
        assert_eq!(dedup.get("f", t(7 * 3600)), None);
        assert!(dedup.peek("f").is_none());
    }

    #[test]
    fn with_a_ttl_of_zero_every_occurrence_is_a_new_entry_not_notified() {
        let mut dedup = Dedup::new(Duration::ZERO, 10);
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(0));
        assert!(dedup.mark_notified("f", &explanation("narrow")));
        // The next occurrence finds no fresh entry, so it calls the model and stores again.
        assert_eq!(dedup.get("f", t(0)), None);
        dedup.put("f", explanation("full"), Some(explanation("narrow")), t(1));
        assert!(!dedup.already_sent("f", &explanation("narrow")));
    }
}
