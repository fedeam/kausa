//! Cost controls (docs/explanation/architecture.md, "Cost controls", and
//! docs/adr/0021-refill-the-call-budget.md): a budget of model calls, and a cache of explanations
//! by incident fingerprint, so that a repeated incident is explained again without a new call.
//!
//! The budget is a refill bucket under two sliding windows. With `N` calls an hour
//! (`--max-calls-per-hour`), the bucket starts with a burst of [`START_BURST`] calls (`N` if
//! fewer), gains one call every `3600 / N` seconds and holds at most `N`. A call may start only
//! when the bucket holds a whole call, fewer than `N` calls started in the last 60 minutes, and
//! fewer than `--max-calls-per-day` in the last 24 hours.
//!
//! Both the budget and the cache live in memory: nothing is stored or read back. A new process
//! starts with the burst, and a term start (a takeover with `--leader-lease`) cuts the credit to
//! it, so a restart or a takeover brings at most the burst; the windows count the calls of one
//! process. The hard limit on cost stays the one set on the provider's key (SECURITY.md). Both
//! measure time with the monotonic clock, so a change of the wall clock neither frees nor freezes
//! the budget; the bucket's arithmetic is checked and, should it ever overflow, refuses the call.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::prompt::Explanation;

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);

/// The calls a process may start at once when it starts and when a term starts, before the
/// bucket refills (at most `--max-calls-per-hour`).
pub const START_BURST: usize = 3;

/// Why a call may not start yet: the refill bucket or the 60-minute cap holds it.
pub const HOURLY_SPENT: &str = "the hourly call budget is spent";
const DAILY_SPENT: &str = "the daily call budget is spent";

/// Whether a model call may start, as [`Budget::check`] sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allowance {
    /// A call may start now.
    Now,
    /// Only the bucket or the 60-minute cap stops the call; one may start at `until`.
    Wait { until: Instant },
    /// The daily cap is spent, or a cap is `0`: waiting would not help. The reason is the message
    /// of the skip.
    Never(&'static str),
}

/// The call budget of one process: a refill bucket (one call every `I = 3600 s / per_hour`, at
/// most `per_hour` calls) under a cap of `per_hour` calls in any 60 minutes and `per_day` in any
/// 24 hours. `0` means no call at all.
#[derive(Debug)]
pub struct Budget {
    per_hour: usize,
    per_day: usize,
    /// The start times of the calls of the last day, oldest first (never out of order).
    calls: VecDeque<Instant>,
    /// When the bucket would be full: the credit at `now` is `per_hour × I` minus the time from
    /// `now` to `full_at`, if `full_at` is later. `None` once the arithmetic overflowed, which no
    /// monotonic clock reaches: then no call starts. Unused with an hourly cap of `0`.
    full_at: Option<Instant>,
}

impl Budget {
    /// A budget whose bucket holds the start burst at `now`.
    pub fn new(per_hour: usize, per_day: usize, now: Instant) -> Self {
        let mut budget = Self {
            per_hour,
            per_day,
            calls: VecDeque::new(),
            full_at: Some(now),
        };
        budget.full_at = budget.burst_full_at(now);
        budget
    }

    /// Whether a call may start at `now`; changes nothing.
    pub fn check(&self, now: Instant) -> Allowance {
        if self.per_hour == 0 {
            return Allowance::Never(HOURLY_SPENT);
        }
        if self.per_day == 0 || self.within(now, DAY) >= self.per_day {
            return Allowance::Never(DAILY_SPENT);
        }
        match self.earliest(now) {
            Some(at) if at <= now => Allowance::Now,
            Some(until) => Allowance::Wait { until },
            None => Allowance::Wait { until: now },
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
        match self.check(now) {
            Allowance::Now => {}
            Allowance::Wait { .. } => return Err(HOURLY_SPENT),
            Allowance::Never(why) => return Err(why),
        }
        let Some(full_at) = self
            .full_at
            .zip(self.interval())
            .and_then(|(full_at, interval)| full_at.max(now).checked_add(interval))
        else {
            return Err(HOURLY_SPENT);
        };
        self.full_at = Some(full_at);
        // Logged at the latest instant seen, so that the log stays sorted.
        let at = self.calls.back().map_or(now, |&last| last.max(now));
        self.calls.push_back(at);
        Ok(())
    }

    /// Gives back the call taken last, and its credit, for a request that never reached the
    /// endpoint.
    pub fn refund(&mut self) {
        if self.calls.pop_back().is_none() {
            return;
        }
        if let Some((full_at, interval)) = self.full_at.zip(self.interval()) {
            self.full_at = Some(full_at.checked_sub(interval).unwrap_or(full_at));
        }
    }

    /// Cuts the credit to at most the start burst when a term starts at `now`; never raises it.
    /// The windows are kept.
    pub fn start_term(&mut self, now: Instant) {
        self.full_at = self
            .full_at
            .zip(self.burst_full_at(now))
            .map(|(full_at, cut)| full_at.max(cut));
    }

    /// The calls this process may start at `now`: by the hour, the whole calls in the bucket within
    /// the 60-minute cap, at most the start burst when not `leading` (what a takeover would bring);
    /// by the day, the daily cap minus the calls of the last 24 hours.
    pub fn remaining(&self, now: Instant, leading: bool) -> (u64, u64) {
        let mut hour = self
            .per_hour
            .saturating_sub(self.within(now, HOUR))
            .min(self.whole_calls(now));
        if !leading {
            hour = hour.min(self.burst());
        }
        let day = self.per_day.saturating_sub(self.within(now, DAY));
        (
            u64::try_from(hour).unwrap_or(u64::MAX),
            u64::try_from(day).unwrap_or(u64::MAX),
        )
    }

    /// The credit at `now`, in nanoseconds: `per_hour × I`, less the time until the bucket is full.
    #[cfg(test)]
    fn credit_ns(&self, now: Instant) -> u128 {
        let Some((full_at, interval)) = self.full_at.zip(self.interval()) else {
            return 0;
        };
        (self.per_hour as u128 * interval.as_nanos())
            .saturating_sub(full_at.saturating_duration_since(now).as_nanos())
    }

    /// `B`: the start burst, at most the hourly cap.
    fn burst(&self) -> usize {
        START_BURST.min(self.per_hour)
    }

    /// `I`: one hour over the hourly cap, in whole nanoseconds; `None` for a cap of `0` (or one
    /// too large for a nanosecond).
    fn interval(&self) -> Option<Duration> {
        let per_hour = u32::try_from(self.per_hour).ok()?;
        HOUR.checked_div(per_hour).filter(|i| !i.is_zero())
    }

    /// `calls × I`.
    fn span(&self, calls: usize) -> Option<Duration> {
        self.interval()?.checked_mul(u32::try_from(calls).ok()?)
    }

    /// When the bucket is full if it holds the start burst at `now`: `now + (per_hour − B) × I`;
    /// `now` with an hourly cap of `0`.
    fn burst_full_at(&self, now: Instant) -> Option<Instant> {
        if self.per_hour == 0 {
            return Some(now);
        }
        now.checked_add(self.span(self.per_hour - self.burst())?)
    }

    /// The calls of the log that count at `now` in a window of `window`. A call counts while it is
    /// younger than the window, and so does one logged later than `now`.
    fn within(&self, now: Instant, window: Duration) -> usize {
        let old = self
            .calls
            .partition_point(|&t| now.saturating_duration_since(t) >= window);
        self.calls.len() - old
    }

    /// The whole calls in the bucket at `now`: `per_hour − ⌈(full_at − now)⁺ / I⌉`, at least 0.
    fn whole_calls(&self, now: Instant) -> usize {
        let Some((full_at, interval)) = self.full_at.zip(self.interval()) else {
            return 0;
        };
        let missing = full_at
            .saturating_duration_since(now)
            .as_nanos()
            .div_ceil(interval.as_nanos());
        usize::try_from((self.per_hour as u128).saturating_sub(missing)).unwrap_or(0)
    }

    /// The first instant, `now` or later, at which the bucket holds a whole call and the 60-minute
    /// cap has a free place; `None` if the arithmetic overflows. The hourly cap is not `0`.
    fn earliest(&self, now: Instant) -> Option<Instant> {
        // The bucket holds a whole call once the time to full is at most (per_hour − 1) × I.
        let to_full = self.full_at?.saturating_duration_since(now);
        let room = self.span(self.per_hour - 1)?;
        let mut at = now.checked_add(to_full.saturating_sub(room))?;
        if self.within(now, HOUR) >= self.per_hour {
            // The oldest of the last per_hour calls leaves the window an hour after it started.
            let oldest = self.calls.get(self.calls.len() - self.per_hour)?;
            at = at.max(oldest.checked_add(HOUR)?);
        }
        Some(at)
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
        if !self.is_fresh(fingerprint, now) {
            self.entries.remove(fingerprint);
            return None;
        }
        self.entries.get(fingerprint).cloned()
    }

    /// Whether `fingerprint` has an entry that has not expired at `now`: the test of `get`, which
    /// removes nothing and clones nothing.
    pub fn is_fresh(&self, fingerprint: &str, now: Instant) -> bool {
        self.entries
            .get(fingerprint)
            .is_some_and(|c| now.saturating_duration_since(c.stored_at) < self.ttl)
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
        let mut budget = Budget::new(2, 100, t(0));
        assert!(budget.take(t(0)).is_ok());
        assert!(budget.take(t(10)).is_ok());
        assert_eq!(budget.take(t(20)), Err("the hourly call budget is spent"));
        assert!(budget.take(t(3600)).is_ok()); // the first call is an hour old
    }

    #[test]
    fn the_daily_cap_holds() {
        let mut budget = Budget::new(10, 3, t(0));
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
        let mut budget = Budget::new(1, 10, t(0));
        assert!(budget.take(t(100)).is_ok());
        assert!(budget.take(t(50)).is_err());
    }

    #[test]
    fn a_refunded_call_frees_its_place() {
        let mut budget = Budget::new(1, 10, t(0));
        assert!(budget.take(t(0)).is_ok());
        budget.refund();
        assert!(budget.take(t(1)).is_ok());
        assert!(budget.take(t(2)).is_err());
    }

    #[test]
    fn a_zero_budget_allows_nothing() {
        assert!(Budget::new(0, 0, t(0)).take(t(0)).is_err());
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

    // Acceptance test for v0.6's self-observability (no ADR), written before the code. It uses
    // `Dedup::count(&self) -> usize`: the entries held, expired ones included until a `get` reads
    // them or a `put` evicts them; `kausa_dedup_cache_entries` shows it.
    #[test]
    fn count_follows_puts_evictions_and_expired_reads() {
        let mut dedup = Dedup::new(Duration::from_secs(60), 2);
        assert_eq!(dedup.count(), 0);
        dedup.put("a", explanation("a"), None, t(0));
        assert_eq!(dedup.count(), 1);
        // A put that replaces an entry adds none.
        dedup.put("a", explanation("a again"), None, t(1));
        assert_eq!(dedup.count(), 1);
        dedup.put("b", explanation("b"), None, t(2));
        assert_eq!(dedup.count(), 2);
        // At capacity, a new entry evicts the oldest.
        dedup.put("c", explanation("c"), None, t(3));
        assert_eq!(dedup.count(), 2);
        assert!(dedup.peek("a").is_none(), "the oldest was evicted");
        // An expired entry is counted until a get reads it: is_fresh and peek remove nothing.
        assert!(!dedup.is_fresh("b", t(100)));
        assert!(dedup.peek("b").is_some());
        assert_eq!(dedup.count(), 2);
        assert!(dedup.get("b", t(100)).is_none());
        assert_eq!(dedup.count(), 1);
        assert!(dedup.get("c", t(10)).is_some());
        assert_eq!(dedup.count(), 1);
        // A cache of capacity 0 holds nothing.
        let mut none = Dedup::new(Duration::from_secs(60), 0);
        none.put("a", explanation("a"), None, t(0));
        assert_eq!(none.count(), 0);
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

    // Acceptance tests for the refill bucket (docs/adr/0021-refill-the-call-budget.md), written
    // before the code. They use this API, which budget.rs does not have yet:
    //
    // - `pub const START_BURST: usize = 3`;
    // - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Allowance { Now, Wait { until:
    //   Instant }, Never(&'static str) }`;
    // - `Budget::new(per_hour, per_day, now: Instant)`: the credit starts at
    //   `B = min(START_BURST, per_hour)` calls, one call accrues every `I = 3600 s / per_hour`
    //   (whole nanoseconds), up to `per_hour` calls;
    // - `Budget::check(&self, now) -> Allowance`, which changes nothing: `Never` for a cap of 0 or
    //   a spent daily cap (the hourly message first for a zero hourly cap), `Wait { until }` with
    //   the later of the instants at which the bucket holds a whole call and the 60-minute cap
    //   frees a place, `Now` otherwise;
    // - `take` and `refund` with today's signatures: `take` refuses a `Wait` with the hourly
    //   message, and `refund` gives back the call and its credit;
    // - `Budget::start_term(&mut self, now)`: the credit is cut to at most `B`, never raised;
    // - `Budget::remaining(&self, now, leading: bool) -> (u64, u64)`: (whole calls in the bucket
    //   within the 60-minute cap, cut to `B` when not leading; the daily cap minus the calls of
    //   the last 24 hours);
    // - `#[cfg(test)] Budget::credit_ns(&self, now) -> u128`: the credit in nanoseconds,
    //   `per_hour × I − (full_at − now)⁺`;
    // - `Dedup::is_fresh(&self, fingerprint, now) -> bool`: the TTL test of `get`, removing
    //   nothing.

    const HOURLY: &str = "the hourly call budget is spent";
    const DAILY: &str = "the daily call budget is spent";

    /// `t(0)` plus `nanos` nanoseconds.
    fn tn(nanos: u128) -> Instant {
        t(0) + Duration::from_nanos(u64::try_from(nanos).expect("a short schedule"))
    }

    /// Takes `n` calls at `now`, each of which must succeed.
    fn take_n(budget: &mut Budget, n: usize, now: Instant) {
        for i in 0..n {
            assert_eq!(budget.take(now), Ok(()), "call {i} of {n}");
        }
    }

    #[test]
    fn a_start_allows_the_burst_then_one_call_per_interval() {
        assert_eq!(START_BURST, 3);
        // 30 an hour: one call every 120 s after the first three.
        let mut budget = Budget::new(30, 200, t(0));
        assert_eq!(budget.check(t(0)), Allowance::Now);
        take_n(&mut budget, 3, t(0));
        assert_eq!(budget.check(t(0)), Allowance::Wait { until: t(120) });
        assert_eq!(budget.take(t(0)), Err(HOURLY));
        assert_eq!(budget.take(t(119)), Err(HOURLY));
        assert_eq!(budget.check(t(120)), Allowance::Now);
        assert_eq!(budget.take(t(120)), Ok(()));
        assert_eq!(budget.check(t(120)), Allowance::Wait { until: t(240) });
        assert_eq!(budget.take(t(239)), Err(HOURLY));
        assert_eq!(budget.take(t(240)), Ok(()));
    }

    #[test]
    fn a_small_hourly_cap_bounds_the_burst() {
        // One an hour: a burst of 1; the bucket and the 60-minute cap both free it at t(3600).
        let mut budget = Budget::new(1, 200, t(0));
        take_n(&mut budget, 1, t(0));
        assert_eq!(budget.take(t(0)), Err(HOURLY));
        assert_eq!(budget.check(t(0)), Allowance::Wait { until: t(3600) });
        assert_eq!(budget.take(t(3599)), Err(HOURLY));
        assert_eq!(budget.take(t(3600)), Ok(()));
        // Two an hour: a burst of 2; the bucket holds a call again at t(1800), but the 60-minute
        // cap only at t(3600), the later of the two.
        let mut budget = Budget::new(2, 200, t(0));
        take_n(&mut budget, 2, t(0));
        assert_eq!(budget.take(t(0)), Err(HOURLY));
        assert_eq!(budget.check(t(0)), Allowance::Wait { until: t(3600) });
        assert_eq!(budget.take(t(1800)), Err(HOURLY));
        assert_eq!(budget.take(t(3600)), Ok(()));
    }

    #[test]
    fn credit_stops_at_the_hourly_cap_after_a_long_quiet_spell() {
        let mut budget = Budget::new(30, 200, t(0));
        let later = t(10 * 3600);
        take_n(&mut budget, 30, later);
        assert_eq!(budget.take(later), Err(HOURLY));
        assert_eq!(
            budget.check(later),
            Allowance::Wait {
                until: t(10 * 3600 + 3600)
            }
        );
    }

    #[test]
    fn the_sixty_minute_cap_binds_a_full_bucket() {
        let mut budget = Budget::new(30, 200, t(0));
        take_n(&mut budget, 30, t(4000));
        // At t(7000) the bucket holds 25 calls again, but 30 started in the last 60 minutes.
        assert_eq!(budget.remaining(t(7000), true).0, 0);
        assert_eq!(budget.check(t(7000)), Allowance::Wait { until: t(7600) });
        assert_eq!(budget.take(t(5000)), Err(HOURLY));
        assert_eq!(budget.take(t(7599)), Err(HOURLY));
        assert_eq!(budget.check(t(7600)), Allowance::Now);
        assert_eq!(budget.take(t(7600)), Ok(()));
    }

    #[test]
    fn the_daily_cap_never_waits() {
        // Alone: the bucket is full and the hour is free, but the day is spent.
        let mut budget = Budget::new(30, 2, t(0));
        take_n(&mut budget, 2, t(0));
        assert_eq!(budget.check(t(0)), Allowance::Never(DAILY));
        assert_eq!(budget.check(t(5 * 3600)), Allowance::Never(DAILY));
        assert_eq!(budget.take(t(5 * 3600)), Err(DAILY));
        assert_eq!(budget.check(t(86_400)), Allowance::Now);
        // With the hourly limit reached too, the daily message wins: waiting would not help.
        let mut budget = Budget::new(2, 2, t(0));
        take_n(&mut budget, 2, t(0));
        assert_eq!(budget.check(t(0)), Allowance::Never(DAILY));
        assert_eq!(budget.take(t(0)), Err(DAILY));
    }

    #[test]
    fn zero_caps_never_allow() {
        let budget = Budget::new(0, 10, t(0));
        assert_eq!(budget.check(t(0)), Allowance::Never(HOURLY));
        assert_eq!(budget.check(t(10 * 3600)), Allowance::Never(HOURLY));
        assert_eq!(budget.remaining(t(0), true).0, 0);
        assert_eq!(budget.remaining(t(0), false).0, 0);
        let budget = Budget::new(10, 0, t(0));
        assert_eq!(budget.check(t(0)), Allowance::Never(DAILY));
        assert_eq!(budget.remaining(t(0), true).1, 0);
        // Both 0: the hourly message, as today.
        assert_eq!(
            Budget::new(0, 0, t(0)).check(t(0)),
            Allowance::Never(HOURLY)
        );
        for (per_hour, per_day, why) in [(0, 10, HOURLY), (10, 0, DAILY), (0, 0, HOURLY)] {
            let mut budget = Budget::new(per_hour, per_day, t(0));
            assert_eq!(budget.take(t(0)), Err(why), "{per_hour}/{per_day}");
            assert_eq!(budget.take(t(10 * 3600)), Err(why), "{per_hour}/{per_day}");
            // A refund with nothing taken changes nothing.
            budget.refund();
            assert_eq!(budget.take(t(10 * 3600)), Err(why), "{per_hour}/{per_day}");
        }
    }

    #[test]
    fn a_refund_gives_back_the_call_and_its_credit() {
        let mut budget = Budget::new(30, 200, t(0));
        take_n(&mut budget, 3, t(0));
        assert_eq!(budget.remaining(t(0), true), (0, 197));
        budget.refund();
        assert_eq!(budget.remaining(t(0), true), (1, 198));
        assert_eq!(budget.check(t(0)), Allowance::Now);
        assert_eq!(budget.take(t(0)), Ok(()));
        assert_eq!(budget.take(t(0)), Err(HOURLY));
        // Mid-bucket, a take and its refund leave the credit exactly as it was.
        let mut budget = Budget::new(30, 200, t(0));
        let before = budget.credit_ns(t(50));
        assert_eq!(budget.take(t(50)), Ok(()));
        assert_eq!(budget.credit_ns(t(50)) + 120_000_000_000, before);
        budget.refund();
        assert_eq!(budget.credit_ns(t(50)), before);
        // With a full bucket, the refund cannot raise the credit past the cap.
        let mut budget = Budget::new(30, 200, t(0));
        assert_eq!(budget.take(t(36_000)), Ok(()));
        budget.refund();
        assert_eq!(budget.credit_ns(t(36_000)), 30 * 120_000_000_000);
        take_n(&mut budget, 30, t(36_000));
        assert_eq!(budget.take(t(36_000)), Err(HOURLY));
        // A refund with nothing taken changes nothing.
        let mut budget = Budget::new(30, 200, t(0));
        let before = budget.credit_ns(t(10));
        budget.refund();
        assert_eq!(budget.credit_ns(t(10)), before);
    }

    #[test]
    fn a_term_start_cuts_the_credit_to_the_burst_and_never_raises_it() {
        // After ten quiet hours the bucket is full; a term start cuts it to 3.
        let mut budget = Budget::new(30, 200, t(0));
        let later = t(10 * 3600);
        assert_eq!(budget.remaining(later, true).0, 30);
        budget.start_term(later);
        assert_eq!(budget.credit_ns(later), 3 * 120_000_000_000);
        take_n(&mut budget, 3, later);
        assert_eq!(budget.take(later), Err(HOURLY));
        // An empty bucket stays as it is.
        let mut budget = Budget::new(30, 200, t(0));
        take_n(&mut budget, 3, t(0));
        let before = budget.credit_ns(t(60));
        budget.start_term(t(60));
        assert_eq!(budget.credit_ns(t(60)), before);
        assert_eq!(budget.check(t(60)), Allowance::Wait { until: t(120) });
        // A bucket below the burst stays as it is too.
        let mut budget = Budget::new(30, 200, t(0));
        take_n(&mut budget, 1, t(0));
        let before = budget.credit_ns(t(30));
        budget.start_term(t(30));
        assert_eq!(budget.credit_ns(t(30)), before);
        // The windows are kept: a term start frees no place in the 60-minute or 24-hour cap.
        let mut budget = Budget::new(2, 3, t(0));
        take_n(&mut budget, 2, t(0));
        budget.start_term(t(10));
        assert_eq!(budget.check(t(10)), Allowance::Wait { until: t(3600) });
        assert_eq!(budget.remaining(t(10), true), (0, 1));
        assert_eq!(budget.take(t(3600)), Ok(()));
        budget.start_term(t(7200));
        assert_eq!(budget.check(t(7200)), Allowance::Never(DAILY));
    }

    #[test]
    fn remaining_counts_both_windows_and_a_standby_sees_at_most_the_burst() {
        let mut budget = Budget::new(30, 200, t(0));
        assert_eq!(budget.remaining(t(0), true), (3, 200));
        assert_eq!(budget.remaining(t(0), false), (3, 200));
        take_n(&mut budget, 2, t(0));
        assert_eq!(budget.remaining(t(0), true), (1, 198));
        assert_eq!(budget.remaining(t(0), false), (1, 198));
        // Ten hours later the bucket is full: what a takeover would bring is still 3.
        let later = 10 * 3600;
        assert_eq!(budget.remaining(t(later), true), (30, 198));
        assert_eq!(budget.remaining(t(later), false), (3, 198));
        take_n(&mut budget, 28, t(later));
        assert_eq!(budget.remaining(t(later), true), (2, 170));
        assert_eq!(budget.remaining(t(later), false), (2, 170));
        take_n(&mut budget, 2, t(later));
        assert_eq!(budget.remaining(t(later), true), (0, 168));
        // 50 minutes on, the bucket holds 25 calls, but the 60-minute cap allows none.
        assert_eq!(budget.remaining(t(later + 3000), true), (0, 168));
        // An hour on, the 60-minute cap is free again; the day still counts all 32 calls.
        assert_eq!(budget.remaining(t(later + 3600), true), (30, 168));
        // A day after each call, it leaves the daily window.
        assert_eq!(budget.remaining(t(86_400), true).1, 170);
        assert_eq!(budget.remaining(t(later + 86_400), true).1, 200);
        // A burst of 2 for two an hour: a standby sees at most 2.
        let budget = Budget::new(2, 200, t(0));
        assert_eq!(budget.remaining(t(later), false), (2, 200));
    }

    #[test]
    fn a_call_at_an_earlier_instant_is_logged_at_the_latest_one() {
        // The call log stays sorted, so both windows can count it with a binary search: a take at
        // an instant earlier than the last call counts as made at the last call's instant.
        let mut budget = Budget::new(30, 200, t(0));
        take_n(&mut budget, 1, t(10_000));
        take_n(&mut budget, 20, t(9000));
        assert_eq!(budget.take(t(9000)), Err(HOURLY));
        // At t(12_700) the bucket is full again, and all 21 calls are 2700 s old.
        assert_eq!(budget.remaining(t(12_700), true), (9, 179));
        assert_eq!(budget.remaining(t(13_600), true), (30, 179));
    }

    #[test]
    fn check_agrees_with_take() {
        for (per_hour, per_day) in [(30, 200), (2, 100), (10, 3), (1, 1), (7, 10_000)] {
            let mut budget = Budget::new(per_hour, per_day, t(0));
            for step in 0..2600 {
                let now = t(step * 37);
                let allowance = budget.check(now);
                // check changes nothing.
                assert_eq!(
                    budget.check(now),
                    allowance,
                    "{per_hour}/{per_day} at {step}"
                );
                if let Allowance::Wait { until } = allowance {
                    assert!(until > now, "{per_hour}/{per_day} at {step}");
                    // At `until`, a call may start, unless the day is spent by then.
                    assert!(
                        !matches!(budget.check(until), Allowance::Wait { .. }),
                        "{per_hour}/{per_day} at {step}"
                    );
                }
                let expected = match allowance {
                    Allowance::Now => Ok(()),
                    Allowance::Wait { .. } => Err(HOURLY),
                    Allowance::Never(why) => Err(why),
                };
                assert_eq!(budget.take(now), expected, "{per_hour}/{per_day} at {step}");
            }
        }
    }

    #[test]
    fn the_largest_cap_does_not_overflow() {
        const MILLION: usize = 1_000_000;
        // One call every 3.6 ms after the first three.
        let interval = Duration::from_secs(3600) / 1_000_000;
        let mut budget = Budget::new(MILLION, MILLION, t(0));
        assert_eq!(budget.remaining(t(0), true), (3, 1_000_000));
        for k in 0..1000u32 {
            assert_eq!(budget.take(t(0) + interval * k), Ok(()), "call {k}");
        }
        let end = t(0) + interval * 1000;
        assert_eq!(budget.remaining(end, true).1, 999_000);
        budget.start_term(end);
        assert!(budget.remaining(end, true).0 <= 3);
        assert_eq!(budget.remaining(t(10 * 3600), true), (1_000_000, 999_000));
        assert_eq!(budget.check(t(10 * 3600)), Allowance::Now);
    }

    #[test]
    fn is_fresh_applies_the_ttl_and_removes_nothing() {
        let mut dedup = Dedup::new(TTL, 10);
        dedup.put("f", explanation("a"), None, t(0));
        assert!(dedup.is_fresh("f", t(0)));
        assert!(dedup.is_fresh("f", t(6 * 3600 - 1)));
        assert!(!dedup.is_fresh("f", t(6 * 3600)));
        assert!(!dedup.is_fresh("missing", t(0)));
        // An expired entry is still there for peek, and get still expires it.
        assert!(dedup.peek("f").is_some());
        assert!(dedup.is_fresh("f", t(1)));
        assert_eq!(dedup.get("f", t(6 * 3600)), None);
        assert!(!dedup.is_fresh("f", t(1)));
        // A TTL of 0 never answers from the cache.
        let mut dedup = Dedup::new(Duration::ZERO, 10);
        dedup.put("f", explanation("a"), None, t(0));
        assert!(!dedup.is_fresh("f", t(0)));
        assert!(dedup.peek("f").is_some());
    }

    /// A 64-bit xorshift: deterministic schedules with no new crate.
    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: u128) -> u128 {
            u128::from(self.next()) % n.max(1)
        }
    }

    /// What one step of a schedule did, and the credit it is measured from.
    struct Step {
        at: u128,
        /// The credit, in nanoseconds, before the step; after it for a term start, a restart or a
        /// refund, which change it without a call.
        credit: u128,
        call: bool,
        start: bool,
    }

    #[test]
    fn the_bound_holds_on_seeded_schedules() {
        const HOUR_NS: u128 = 3600 * 1_000_000_000;
        const DAY_NS: u128 = 24 * HOUR_NS;
        const SPAN_NS: u128 = 3 * HOUR_NS;
        for per_hour in [1usize, 2, 3, 7, 30, 360] {
            for per_day in [per_hour, 200, 10_000] {
                let interval = (Duration::from_secs(3600) / per_hour as u32).as_nanos();
                let burst = START_BURST.min(per_hour) as u128;
                let capacity = per_hour as u128 * interval;
                // Steps of at most four intervals, and of at most 40 minutes.
                let longest = (4 * interval).min(2400 * 1_000_000_000);
                for seed in 0..200u64 {
                    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15 ^ (seed + 1).wrapping_mul(0xD1B5));
                    let case = format!("{per_hour}/h {per_day}/day seed {seed}");
                    let mut now = 0u128;
                    let mut budget = Budget::new(per_hour, per_day, tn(0));
                    // The successful calls of each process, and the step of the last one while
                    // it may still be refunded.
                    let mut processes: Vec<Vec<u128>> = vec![Vec::new()];
                    let mut refundable: Option<usize> = None;
                    let mut steps = vec![Step {
                        at: 0,
                        credit: budget.credit_ns(tn(0)),
                        call: false,
                        start: true,
                    }];
                    assert!(steps[0].credit <= burst * interval, "{case}: a start");
                    while now < SPAN_NS {
                        now += match rng.below(4) {
                            0 => 0,
                            1 => 1 + rng.below(1000),
                            _ => rng.below(longest),
                        };
                        let at = tn(now);
                        let before = budget.credit_ns(at);
                        assert!(before <= capacity, "{case}: past the capacity");
                        let mut step = Step {
                            at: now,
                            credit: before,
                            call: false,
                            start: false,
                        };
                        match rng.below(100) {
                            0..=9 if refundable.is_some() => {
                                budget.refund();
                                let index = refundable.take().expect("a refundable call");
                                steps[index].call = false;
                                processes.last_mut().expect("a process").pop();
                                step.credit = budget.credit_ns(at);
                            }
                            10..=14 => {
                                budget.start_term(at);
                                step.credit = budget.credit_ns(at);
                                step.start = true;
                                refundable = None;
                                assert!(step.credit <= before, "{case}: a term start raised it");
                            }
                            15..=17 => {
                                budget = Budget::new(per_hour, per_day, at);
                                processes.push(Vec::new());
                                step.credit = budget.credit_ns(at);
                                step.start = true;
                                refundable = None;
                            }
                            _ => {
                                let allowance = budget.check(at);
                                let taken = budget.take(at);
                                assert_eq!(
                                    taken.is_ok(),
                                    allowance == Allowance::Now,
                                    "{case}: check and take disagree"
                                );
                                if taken.is_ok() {
                                    assert!(before >= interval, "{case}: a call without credit");
                                    step.call = true;
                                    refundable = Some(steps.len());
                                    processes.last_mut().expect("a process").push(now);
                                } else {
                                    refundable = None;
                                }
                            }
                        }
                        if step.start {
                            assert!(step.credit <= burst * interval, "{case}: a start");
                        }
                        steps.push(step);
                    }
                    // For every pair of steps i <= j, in exact nanoseconds:
                    //   calls(i..=j) × I <= credit(i) + B × K × I + (at(j) − at(i)),
                    // with K the term starts in (i, j]. With prefix sums
                    //   A(j) = calls(..=j) × I − B × I × starts(..=j) − at(j)
                    //   R(i) = credit(i) + calls(..i) × I − B × I × starts(..=i) − at(i),
                    // it is max(A(j) for j >= i) <= R(i) for every i.
                    let interval = interval as i128;
                    let burst = burst as i128;
                    let (mut calls, mut starts) = (0i128, 0i128);
                    let mut a = Vec::with_capacity(steps.len());
                    let mut r = Vec::with_capacity(steps.len());
                    for step in &steps {
                        let calls_before = calls;
                        calls += i128::from(step.call);
                        starts += i128::from(step.start);
                        let at = step.at as i128;
                        a.push(calls * interval - burst * interval * starts - at);
                        r.push(
                            step.credit as i128 + calls_before * interval
                                - burst * interval * starts
                                - at,
                        );
                    }
                    let mut most = i128::MIN;
                    for (i, (a, r)) in a.iter().zip(&r).enumerate().rev() {
                        most = most.max(*a);
                        assert!(most <= *r, "{case}: the bound fails from step {i}");
                    }
                    // Each process: at most per_hour calls in any 60 minutes, and per_day in any
                    // 24 hours.
                    for calls in &processes {
                        for (cap, window) in [(per_hour, HOUR_NS), (per_day, DAY_NS)] {
                            for pair in calls.windows(cap + 1) {
                                assert!(
                                    pair[cap] - pair[0] >= window,
                                    "{case}: more than {cap} calls in a window"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
