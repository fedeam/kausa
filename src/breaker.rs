//! The breaker in front of the model endpoint (docs/adr/0020-check-the-endpoint.md): after 3
//! transient failures in a row, or 1 configuration error, kausa holds its calls, then lets one
//! probe through after each hold, doubled up to 30 minutes while probes fail. Pure: `now` is
//! passed in, as in `src/budget.rs`, and no `crate::` import.
//!
//! The caller maps each call's outcome to a [`Call`]: a chat completion, valid or not, is
//! [`Call::Answer`]; a failure of the `transient` or `configuration` class is
//! [`Call::Transient`] or [`Call::Configuration`], with its clamped `Retry-After`; a request a
//! tenant shaped (`rejected`, `too_large`, `no_content`) is [`Call::Neutral`] and changes nothing,
//! so it neither opens nor closes the breaker.
//!
//! The breaker never starts or refuses a call itself: [`Breaker::state`] tells the slot whether
//! calls are held ([`State::Open`]), whether the next call is the one probe ([`State::HalfOpen`]),
//! or neither ([`State::Closed`]).

use std::time::{Duration, Instant};

/// Consecutive transient failures, while closed, that open the breaker.
pub const OPEN_AFTER: u32 = 3;

/// The first hold, and the one after each close.
pub const FIRST_HOLD: Duration = Duration::from_secs(60);

/// The longest hold a failed probe doubles to. Never shorter than the longest `Retry-After`
/// kausa honours (a test in src/model.rs ties them).
pub const MAX_HOLD: Duration = Duration::from_secs(1800);

/// Where the breaker stands at an instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Calls go through.
    Closed,
    /// Calls are held until [`Breaker::open_until`].
    Open,
    /// The hold is over: the next call is the one probe.
    HalfOpen,
}

/// The outcome of one call, as the breaker counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// A chat completion, valid or not.
    Answer,
    /// A request the endpoint refused for what it held (`rejected`, `too_large`, `no_content`).
    Neutral,
    /// A failure that may pass by itself (`connection`, `timeout`, `rate_limited`,
    /// `server_error`), with its clamped `Retry-After`.
    Transient { retry_after: Option<Duration> },
    /// A failure that needs a person (`redirect`, `not_an_answer`, `quota`, `key`, `not_found`),
    /// with its clamped `Retry-After`.
    Configuration { retry_after: Option<Duration> },
}

/// What a recorded call changed, for the `endpoint unavailable` and `endpoint available` lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    /// Nothing the logs report.
    None,
    /// The breaker opened, or a failed probe reopened it, for `hold` (the whole hold,
    /// `Retry-After` included); `failed_calls` counts the failed calls since the outage began.
    Opened { hold: Duration, failed_calls: u32 },
    /// An answer closed it, `unavailable` after it first opened and after `failed_calls` failed
    /// calls.
    Closed {
        unavailable: Duration,
        failed_calls: u32,
    },
}

/// The breaker's state. One per process, kept across terms.
#[derive(Debug)]
pub struct Breaker {
    /// Consecutive transient failures while closed.
    failures: u32,
    /// The end of the current or last hold; `None` while closed.
    open_until: Option<Instant>,
    /// The length of the last hold, before `Retry-After`.
    interval: Duration,
    /// When the outage began: the first opening since the last close.
    opened_at: Option<Instant>,
    /// Failed calls since the outage began, the ones that opened it included.
    failed_calls: u32,
}

impl Breaker {
    /// A closed breaker.
    pub fn new() -> Self {
        Breaker {
            failures: 0,
            open_until: None,
            interval: FIRST_HOLD,
            opened_at: None,
            failed_calls: 0,
        }
    }

    /// `Closed` until it opens; `Open` while `now` is before [`Breaker::open_until`];
    /// `HalfOpen` from that instant until the next call is recorded.
    pub fn state(&self, now: Instant) -> State {
        match self.open_until {
            None => State::Closed,
            Some(until) if now < until => State::Open,
            Some(_) => State::HalfOpen,
        }
    }

    /// The end of the current hold (the probe time), also once it has passed; `None` while
    /// closed.
    pub fn open_until(&self) -> Option<Instant> {
        self.open_until
    }

    /// Counts one call that ended at `now`. A call recorded while `Open` (the slot starts none)
    /// counts as a probe, so there is no panic path.
    pub fn record(&mut self, call: Call, now: Instant) -> Transition {
        match call {
            Call::Neutral => Transition::None,
            Call::Answer => {
                let transition = match self.opened_at {
                    Some(opened_at) if self.open_until.is_some() => Transition::Closed {
                        unavailable: now.saturating_duration_since(opened_at),
                        failed_calls: self.failed_calls,
                    },
                    _ => Transition::None,
                };
                *self = Breaker::new();
                transition
            }
            Call::Transient { retry_after } => {
                if self.open_until.is_some() {
                    return self.reopen(retry_after, now);
                }
                self.failures = self.failures.saturating_add(1);
                if self.failures >= OPEN_AFTER {
                    self.open(retry_after, now)
                } else {
                    Transition::None
                }
            }
            Call::Configuration { retry_after } => {
                if self.open_until.is_some() {
                    return self.reopen(retry_after, now);
                }
                self.failures = self.failures.saturating_add(1);
                self.open(retry_after, now)
            }
        }
    }

    /// Opens a closed breaker for the first hold; the outage counts the failures that opened it.
    fn open(&mut self, retry_after: Option<Duration>, now: Instant) -> Transition {
        self.opened_at = Some(now);
        self.failed_calls = self.failures;
        self.failures = 0;
        self.interval = FIRST_HOLD;
        self.hold(retry_after, now)
    }

    /// Reopens after a failed probe, with the interval doubled up to [`MAX_HOLD`].
    fn reopen(&mut self, retry_after: Option<Duration>, now: Instant) -> Transition {
        self.failed_calls = self.failed_calls.saturating_add(1);
        self.interval = self.interval.saturating_mul(2).min(MAX_HOLD);
        self.hold(retry_after, now)
    }

    /// Holds calls for the interval, or longer when `Retry-After` asks for more; the interval
    /// itself stays, so a `Retry-After` never compounds into the next hold.
    fn hold(&mut self, retry_after: Option<Duration>, now: Instant) -> Transition {
        let hold = self.interval.max(retry_after.unwrap_or(Duration::ZERO));
        // An instant past the clock's range cannot come from a hold of minutes; holding until
        // `now` (half-open at once) keeps the type free of a panic path all the same.
        self.open_until = Some(now.checked_add(hold).unwrap_or(now));
        Transition::Opened {
            hold,
            failed_calls: self.failed_calls,
        }
    }
}

#[cfg(test)]
mod tests {
    // Acceptance tests for docs/adr/0020-check-the-endpoint.md, written before the code. They use
    // this API, which does not exist yet:
    //
    // - `pub const OPEN_AFTER: u32 = 3`, `pub const FIRST_HOLD: Duration` (60 s) and `pub const
    //   MAX_HOLD: Duration` (1800 s);
    // - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum State { Closed, Open, HalfOpen }`;
    // - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Call { Answer, Neutral, Transient
    //   { retry_after: Option<Duration> }, Configuration { retry_after: Option<Duration> } }`;
    // - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Transition { None, Opened { hold:
    //   Duration, failed_calls: u32 }, Closed { unavailable: Duration, failed_calls: u32 } }`,
    //   where `hold` is the whole hold, `Retry-After` included;
    // - `Breaker::new() -> Breaker`, `Breaker::state(&self, now: Instant) -> State`,
    //   `Breaker::open_until(&self) -> Option<Instant>` and `Breaker::record(&mut self, call:
    //   Call, now: Instant) -> Transition`.
    //
    // Instants are injected: every test starts at `Instant::now()` and moves forward by adding.

    use super::*;

    use std::time::{Duration, Instant};

    const TRANSIENT: Call = Call::Transient { retry_after: None };
    const CONFIGURATION: Call = Call::Configuration { retry_after: None };

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn opened(hold: u64, failed_calls: u32) -> Transition {
        Transition::Opened {
            hold: secs(hold),
            failed_calls,
        }
    }

    /// A breaker opened by one configuration error at `at`.
    fn open_at(at: Instant) -> Breaker {
        let mut breaker = Breaker::new();
        assert_eq!(breaker.record(CONFIGURATION, at), opened(60, 1));
        breaker
    }

    #[test]
    fn the_constants_are_those_of_the_adr() {
        assert_eq!(OPEN_AFTER, 3);
        assert_eq!(FIRST_HOLD, secs(60));
        assert_eq!(MAX_HOLD, secs(1800));
    }

    #[test]
    fn a_new_breaker_is_closed() {
        let breaker = Breaker::new();
        assert_eq!(breaker.state(Instant::now()), State::Closed);
        assert_eq!(breaker.open_until(), None);
    }

    #[test]
    fn three_transient_failures_open_it_and_two_do_not() {
        let t0 = Instant::now();
        let mut breaker = Breaker::new();
        assert_eq!(breaker.record(TRANSIENT, t0), Transition::None);
        assert_eq!(breaker.record(TRANSIENT, t0 + secs(1)), Transition::None);
        assert_eq!(breaker.state(t0 + secs(1)), State::Closed);
        assert_eq!(breaker.open_until(), None);
        let t3 = t0 + secs(2);
        assert_eq!(breaker.record(TRANSIENT, t3), opened(60, 3));
        assert_eq!(breaker.state(t3), State::Open);
        assert_eq!(breaker.open_until(), Some(t3 + secs(60)));
    }

    #[test]
    fn an_answer_between_failures_resets_the_count() {
        let t0 = Instant::now();
        let mut breaker = Breaker::new();
        breaker.record(TRANSIENT, t0);
        breaker.record(TRANSIENT, t0);
        assert_eq!(breaker.record(Call::Answer, t0), Transition::None);
        assert_eq!(breaker.record(TRANSIENT, t0), Transition::None);
        assert_eq!(breaker.record(TRANSIENT, t0), Transition::None);
        assert_eq!(breaker.state(t0), State::Closed);
        assert_eq!(breaker.record(TRANSIENT, t0), opened(60, 3));
    }

    #[test]
    fn one_configuration_error_opens_it_at_once() {
        let t0 = Instant::now();
        let breaker = open_at(t0);
        assert_eq!(breaker.state(t0), State::Open);
        assert_eq!(breaker.open_until(), Some(t0 + secs(60)));
    }

    #[test]
    fn a_neutral_call_changes_nothing_in_any_state() {
        let t0 = Instant::now();
        // Closed: it neither resets nor counts.
        let mut breaker = Breaker::new();
        breaker.record(TRANSIENT, t0);
        breaker.record(TRANSIENT, t0);
        for _ in 0..10 {
            assert_eq!(breaker.record(Call::Neutral, t0), Transition::None);
        }
        assert_eq!(breaker.state(t0), State::Closed);
        assert_eq!(breaker.record(TRANSIENT, t0), opened(60, 3));

        // Open: the hold stays.
        let mut breaker = open_at(t0);
        assert_eq!(
            breaker.record(Call::Neutral, t0 + secs(10)),
            Transition::None
        );
        assert_eq!(breaker.state(t0 + secs(10)), State::Open);
        assert_eq!(breaker.open_until(), Some(t0 + secs(60)));

        // Half-open: a neutral probe leaves it half-open, with no new hold, and the next failed
        // probe doubles the first hold.
        let mut breaker = open_at(t0);
        let probe = t0 + secs(61);
        assert_eq!(breaker.state(probe), State::HalfOpen);
        for i in 0..10 {
            let at = probe + secs(i);
            assert_eq!(breaker.record(Call::Neutral, at), Transition::None);
            assert_eq!(breaker.state(at), State::HalfOpen);
        }
        assert_eq!(breaker.open_until(), Some(t0 + secs(60)));
        let failed = probe + secs(20);
        assert_eq!(breaker.record(TRANSIENT, failed), opened(120, 2));
        assert_eq!(breaker.open_until(), Some(failed + secs(120)));
    }

    #[test]
    fn open_becomes_half_open_exactly_at_the_end_of_the_hold() {
        let t0 = Instant::now();
        let breaker = open_at(t0);
        let until = breaker.open_until().expect("open");
        assert_eq!(until, t0 + secs(60));
        let just_before = until
            .checked_sub(Duration::from_nanos(1))
            .expect("an earlier instant");
        assert_eq!(breaker.state(just_before), State::Open);
        assert_eq!(breaker.state(until), State::HalfOpen);
        assert_eq!(breaker.state(until + secs(3600)), State::HalfOpen);
    }

    #[test]
    fn each_failed_probe_doubles_the_hold_up_to_thirty_minutes() {
        for first in [TRANSIENT, CONFIGURATION] {
            let mut now = Instant::now();
            let mut breaker = Breaker::new();
            let mut opening = if first == TRANSIENT {
                breaker.record(TRANSIENT, now);
                breaker.record(TRANSIENT, now);
                3
            } else {
                1
            };
            assert_eq!(breaker.record(first, now), opened(60, opening), "{first:?}");
            for hold in [120, 240, 480, 960, 1800, 1800, 1800] {
                now = breaker.open_until().expect("open");
                assert_eq!(breaker.state(now), State::HalfOpen, "{first:?}");
                opening += 1;
                // Either class of failure reopens it from half-open.
                let probe = if hold % 240 == 0 {
                    CONFIGURATION
                } else {
                    TRANSIENT
                };
                assert_eq!(
                    breaker.record(probe, now),
                    opened(hold, opening),
                    "{first:?}: after {opening} failed calls"
                );
                assert_eq!(breaker.open_until(), Some(now + secs(hold)));
                assert_eq!(breaker.state(now), State::Open);
            }
        }
    }

    #[test]
    fn retry_after_raises_a_hold_never_lowers_one_and_does_not_compound() {
        let t0 = Instant::now();
        // A configuration error that says 300 s: the first hold is 300 s.
        let mut breaker = Breaker::new();
        let call = Call::Configuration {
            retry_after: Some(secs(300)),
        };
        assert_eq!(breaker.record(call, t0), opened(300, 1));
        assert_eq!(breaker.open_until(), Some(t0 + secs(300)));
        // The next failed probe doubles the interval (60 s), not the 300 s.
        let probe = t0 + secs(300);
        assert_eq!(breaker.state(probe), State::HalfOpen);
        assert_eq!(breaker.record(TRANSIENT, probe), opened(120, 2));
        // A shorter Retry-After never lowers a hold.
        let probe = probe + secs(120);
        let short = Call::Transient {
            retry_after: Some(secs(30)),
        };
        assert_eq!(breaker.record(short, probe), opened(240, 3));
        assert_eq!(breaker.open_until(), Some(probe + secs(240)));

        // The third transient failure that says 600 s.
        let mut breaker = Breaker::new();
        breaker.record(TRANSIENT, t0);
        breaker.record(TRANSIENT, t0);
        let long = Call::Transient {
            retry_after: Some(secs(600)),
        };
        assert_eq!(breaker.record(long, t0), opened(600, 3));
        assert_eq!(breaker.open_until(), Some(t0 + secs(600)));
        // A Retry-After on a transient failure that does not open it changes nothing.
        let mut breaker = Breaker::new();
        assert_eq!(breaker.record(long, t0), Transition::None);
        assert_eq!(breaker.state(t0), State::Closed);
    }

    #[test]
    fn an_answer_closes_it_reports_the_outage_and_resets_the_hold() {
        let t0 = Instant::now();
        let mut breaker = open_at(t0);
        // A failed probe at 60 s reopens it for 120 s.
        assert_eq!(breaker.record(TRANSIENT, t0 + secs(60)), opened(120, 2));
        // The next probe, 5 s after the hold, is answered: closed after 185 s and 2 failed calls.
        let answered = t0 + secs(185);
        assert_eq!(breaker.state(answered), State::HalfOpen);
        assert_eq!(
            breaker.record(Call::Answer, answered),
            Transition::Closed {
                unavailable: secs(185),
                failed_calls: 2,
            }
        );
        assert_eq!(breaker.state(answered), State::Closed);
        assert_eq!(breaker.open_until(), None);
        // Count and hold start again: two failures do not open it, the third opens for 60 s.
        let later = answered + secs(1);
        assert_eq!(breaker.record(TRANSIENT, later), Transition::None);
        assert_eq!(breaker.record(TRANSIENT, later), Transition::None);
        assert_eq!(breaker.record(TRANSIENT, later), opened(60, 3));
        // An answer while closed is no transition.
        let mut closed = Breaker::new();
        assert_eq!(closed.record(Call::Answer, t0), Transition::None);
    }

    #[test]
    fn an_answer_while_open_closes_it_too() {
        // The slot starts no call while open; the type has no panic path all the same.
        let t0 = Instant::now();
        let mut breaker = open_at(t0);
        assert_eq!(
            breaker.record(Call::Answer, t0 + secs(10)),
            Transition::Closed {
                unavailable: secs(10),
                failed_calls: 1,
            }
        );
        assert_eq!(breaker.state(t0 + secs(10)), State::Closed);
    }
}
