//! Notifications (docs/adr/0015-notifications.md, docs/adr/0027-fact-statement-for-the-detections.md):
//! the webhook's URL check, its client, the queue of messages and their delivery, one at a time,
//! outside the explanation path and the fact path. The bodies are built by `notification.rs`.
//!
//! A `detected` message is queued by the fact path, an `explained` one by the explanation path.
//! The `detected` ones have their own bounds: once per fingerprint per `--dedup-hours`, and at most
//! `--notify-max-per-hour` started in any 60 minutes, over which one fixed `capped` message is
//! posted at most once an hour.
//!
//! The webhook URL and the bearer token are secrets: they are never logged, and no error, reason
//! or `Debug` output carries them. Only the status code of an answer is read, never its body.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::budget::Dedup;
use crate::explain;
use crate::fact;
use crate::incident::Incident;
use crate::leader::Term;
use crate::log;
use crate::metrics::Metrics;
use crate::model::without_url;
use crate::notification::{self, ChatId, Message, Notice};
use crate::prompt::Explanation;
use crate::report::Fact;

/// How many notifications may wait for the webhook. When one more arrives, the oldest `detected`
/// message is dropped, or the new one if it is `detected` and every queued one is `explained`, or
/// else the oldest `explained` one.
pub const QUEUE_LIMIT: usize = 16;
/// The least time between the starts of two sends, so that one burst stays within the rate a chat
/// service takes: one message per second for Slack's incoming webhooks, and for `json`.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// The same for `telegram`: Telegram lets a bot send 20 messages a minute to a group.
pub const TELEGRAM_MIN_INTERVAL: Duration = Duration::from_secs(3);

/// The most fingerprints whose `detected` message is remembered as sent; one more drops the
/// oldest.
pub const SENT_CAPACITY: usize = 1024;
/// The window of `--notify-max-per-hour`, and the least time between two `capped` messages.
pub const CAP_WINDOW: Duration = Duration::from_secs(3600);
/// `--notify-max-per-hour` without the flag, and the values it takes.
pub const DEFAULT_MAX_PER_HOUR: usize = 30;
pub const MAX_PER_HOUR: std::ops::RangeInclusive<usize> = 1..=3600;

/// The time to connect, and the time for a whole request, before it fails as transient.
#[cfg(not(test))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const TIMEOUT: Duration = Duration::from_secs(3);
// An attempt of a deposed leader ends before a standby may take over
// (docs/adr/0019-high-availability.md); the tests check their own TIMEOUT.
#[cfg(not(test))]
const _: () = assert!(TIMEOUT.as_nanos() <= crate::leader::pure::WRITE_BOUND.as_nanos());

/// The wait before the one retry of a transient failure: `explain::RETRY_DELAY`, which the log
/// line states, and a short one in the tests.
#[cfg(not(test))]
const RETRY_WAIT: Duration = explain::RETRY_DELAY;
#[cfg(test)]
const RETRY_WAIT: Duration = Duration::from_millis(100);

const USER_AGENT: &str = concat!("kausa/", env!("CARGO_PKG_VERSION"));

/// The body the webhook takes (`--notify-format`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Slack,
    /// Telegram's `sendMessage`, to this chat.
    Telegram(ChatId),
}

impl Format {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Slack => "slack",
            Self::Telegram(_) => "telegram",
        }
    }

    /// The least time between the starts of two sends in this format.
    pub fn min_interval(&self) -> Duration {
        match self {
            Self::Json | Self::Slack => MIN_INTERVAL,
            Self::Telegram(_) => TELEGRAM_MIN_INTERVAL,
        }
    }
}

/// Checks the webhook URL read from `--notify-url-file`: it parses, it is `https://` (or `http://`
/// with `allow_http`), it carries no user name or password, and for Telegram its path is exactly
/// `/bot<token>/sendMessage` on any host (the query is not checked). The error is the end of the
/// usage error, after "the URL in --notify-url-file '<path>' ", and never carries the URL.
pub fn check_url(text: &str, format: &Format, allow_http: bool) -> Result<Url, &'static str> {
    let url = Url::parse(text).map_err(|_| "is not a valid URL")?;
    let scheme_ok = url.scheme() == "https" || (allow_http && url.scheme() == "http");
    if !scheme_ok {
        return Err("must be https:// (or http:// with --notify-allow-http)");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not carry a user name or password");
    }
    if matches!(format, Format::Telegram(_)) && !is_send_message(url.path()) {
        return Err("must end in /bot<token>/sendMessage with --notify-format telegram");
    }
    Ok(url)
}

/// Whether `path` is `/bot<token>/sendMessage`, with a token that is not empty and has no `/`.
fn is_send_message(path: &str) -> bool {
    path.strip_prefix("/bot")
        .and_then(|rest| rest.strip_suffix("/sendMessage"))
        .is_some_and(|token| !token.is_empty() && !token.contains('/'))
}

/// The client of the one webhook. It has no `Debug`: the URL and the token are secrets.
pub struct Notifier {
    client: reqwest::Client,
    url: Url,
    format: Format,
    /// The bearer token, kept only for the `json` format.
    token: Option<String>,
}

/// Why one attempt failed. `reason` is for the log: it never carries the URL or the token, nor
/// anything the webhook answered but its status code.
#[derive(Debug)]
pub struct Failure {
    pub reason: String,
    /// Worth one more try: a timeout, a failed connection, `429` or a `5xx` status.
    pub transient: bool,
}

impl Notifier {
    /// A client for `url`, checked by `check_url`: rustls, no proxy, no redirects, `https_only`
    /// unless `allow_http`, a 5 s connect and a 10 s total timeout. `token` is used only with the
    /// `json` format. The error is the whole usage error and carries no URL.
    pub fn new(
        url: Url,
        format: Format,
        token: Option<String>,
        allow_http: bool,
    ) -> Result<Self, String> {
        // The provider the Kubernetes client and the model's client use; a second install is
        // refused, and that error is ignored.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!allow_http)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| format!("cannot build the webhook client: {}", without_url(&e)))?;
        let token = token.filter(|_| format == Format::Json);
        Ok(Self {
            client,
            url,
            format,
            token,
        })
    }

    /// The webhook's host, without its port, path or query: what the `started` line shows.
    pub fn host(&self) -> &str {
        self.url.host_str().unwrap_or_default()
    }

    pub fn format(&self) -> &Format {
        &self.format
    }

    /// One attempt: a `POST` of the body of `notice` in this format. Returns the status code of
    /// a `2xx`; a redirect is not followed, and the answer's body is never read.
    pub async fn send(&self, notice: &Notice) -> Result<u16, Failure> {
        let body = match &self.format {
            Format::Json => notification::json_body(notice, env!("CARGO_PKG_VERSION")),
            Format::Slack => notification::slack_body(notice),
            Format::Telegram(chat) => notification::telegram_body(notice, chat),
        };
        self.post(body).await
    }

    /// The same for the fixed `capped` message, with `limit` `detected` messages an hour.
    pub async fn send_capped(&self, limit: usize) -> Result<u16, Failure> {
        let body = match &self.format {
            Format::Json => notification::capped_json_body(limit, env!("CARGO_PKG_VERSION")),
            Format::Slack => notification::capped_slack_body(limit),
            Format::Telegram(chat) => notification::capped_telegram_body(limit, chat),
        };
        self.post(body).await
    }

    /// The same for a message of a lower class: one attempt of its body in this format.
    #[cfg(test)]
    pub async fn send_low(&self, item: &Low) -> Result<u16, Failure> {
        let body = match item {
            Low::Remediation(r) => match &self.format {
                Format::Json => notification::remediation_json_body(r, env!("CARGO_PKG_VERSION")),
                Format::Slack => notification::remediation_slack_body(r),
                Format::Telegram(chat) => notification::remediation_telegram_body(r, chat),
            },
        };
        self.post(body).await
    }

    async fn post(&self, body: Value) -> Result<u16, Failure> {
        let mut request = self
            .client
            .post(self.url.clone())
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| Failure {
            reason: format!("the request failed: {}", without_url(&e)),
            transient: e.is_timeout() || e.is_connect(),
        })?;
        let status = response.status();
        drop(response);
        if status.is_success() {
            return Ok(status.as_u16());
        }
        if status.is_redirection() {
            return Err(Failure {
                reason: "a redirect is not followed: put the final URL in the file".into(),
                transient: false,
            });
        }
        Err(Failure {
            reason: format!("the webhook answered {}", status.as_u16()),
            transient: status.as_u16() == 429 || status.is_server_error(),
        })
    }
}

/// One message waiting for the webhook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    /// The incident's fingerprint: the key of its `Dedup` entry, or of the table of `detected`
    /// messages sent.
    pub fingerprint: String,
    /// For an `explained` message, the explanation as the message restores it (only the namespace
    /// and the top-level owner), which the `Dedup` entry stores as its `notice`; `None` for a
    /// `detected` one.
    pub explanation: Option<Explanation>,
    pub notice: Notice,
}

/// The facts of a statement as a message carries them: without the detection, which the message
/// names apart, and without the container, whose name never leaves in a notification
/// (docs/adr/0027-fact-statement-for-the-detections.md, point 5).
pub fn facts(facts: &fact::Facts) -> notification::Facts {
    notification::Facts {
        reason: facts.reason.map(str::to_owned),
        exit_code: facts.exit_code,
        pods_seen: facts.pods_seen,
        pods_seen_is_minimum: facts.pods_seen_is_minimum,
        pods_now: facts.pods_now,
        revision: facts.revision,
    }
}

/// The `explained` message of `incident` with `narrow`, the explanation restored by
/// `Redactor::restore_only` for the namespace and the top-level owner, and with the facts of its
/// statement (`pods_total` Pods held for its owner).
pub fn queued(incident: &Incident, pods_total: usize, narrow: Explanation) -> Queued {
    let detection = incident.key.detection;
    let notice = Notice {
        message: Message::Explained,
        detection_id: detection.id().to_owned(),
        detection_name: detection.name().to_owned(),
        api_version: incident.owner.api_version.clone(),
        kind: incident.owner.kind.clone(),
        namespace: incident.key.namespace.clone(),
        name: incident.owner.name.clone(),
        fingerprint: incident.fingerprint.clone(),
        // The same string as the incident's log line.
        first_seen: incident.first_seen.to_string(),
        facts: facts(&fact::statement(incident, pods_total).facts),
        summary: narrow.summary.clone(),
        cause: narrow.cause.clone(),
        steps: narrow.steps.clone(),
        confidence: narrow.confidence.clone(),
    };
    Queued {
        fingerprint: incident.fingerprint.clone(),
        explanation: Some(narrow),
        notice,
    }
}

/// The `detected` message of `fact`.
pub fn detected(fact: &Fact) -> Queued {
    let detection = fact.facts.detection;
    let notice = Notice {
        message: Message::Detected,
        detection_id: detection.id().to_owned(),
        detection_name: detection.name().to_owned(),
        api_version: fact.owner.api_version.clone(),
        kind: fact.owner.kind.clone(),
        namespace: fact.namespace.clone(),
        name: fact.owner.name.clone(),
        fingerprint: fact.fingerprint.clone(),
        // The same string as the incident's log line.
        first_seen: fact.first_seen.to_string(),
        facts: facts(&fact.facts),
        summary: String::new(),
        cause: String::new(),
        steps: Vec::new(),
        confidence: String::new(),
    };
    Queued {
        fingerprint: fact.fingerprint.clone(),
        explanation: None,
        notice,
    }
}

/// What the notification slot sends next: a queued message, or the fixed `capped` one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outgoing {
    Message(Box<Queued>),
    Capped,
}

/// The fingerprints whose `detected` message the webhook acknowledged, and when, oldest first, at
/// most `SENT_CAPACITY`. A fingerprint is sent again once `ttl` has passed; with a `ttl` of zero,
/// every occurrence is.
pub struct Sent {
    ttl: Duration,
    entries: VecDeque<(String, Instant)>,
}

impl Sent {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: VecDeque::new(),
        }
    }

    /// Whether the `detected` message of `fingerprint` was acknowledged less than `ttl` ago.
    pub fn holds(&self, fingerprint: &str, now: Instant) -> bool {
        self.entries.iter().any(|(known, at)| {
            known == fingerprint && now.saturating_duration_since(*at) < self.ttl
        })
    }

    /// Marks `fingerprint` acknowledged at `now`, as the newest entry.
    pub fn mark(&mut self, fingerprint: &str, now: Instant) {
        if self.ttl.is_zero() {
            return;
        }
        self.entries.retain(|(known, _)| known != fingerprint);
        if self.entries.len() >= SENT_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((fingerprint.to_owned(), now));
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The starts of `detected` messages in the last `CAP_WINDOW`, when the last `capped` message was
/// started, and whether one is owed: a message was not sent since then.
pub struct Cap {
    limit: usize,
    starts: VecDeque<Instant>,
    last_capped: Option<Instant>,
    owed: bool,
}

impl Cap {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            starts: VecDeque::new(),
            last_capped: None,
            owed: false,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Whether one more `detected` message may start at `now`; if it may, its start is counted.
    pub fn admit(&mut self, now: Instant) -> bool {
        while self
            .starts
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= CAP_WINDOW)
        {
            self.starts.pop_front();
        }
        if self.starts.len() >= self.limit {
            return false;
        }
        self.starts.push_back(now);
        true
    }

    /// Notes that a message was not sent, over the cap or dropped by a bound: the `capped`
    /// message is owed.
    pub fn owe(&mut self) {
        self.owed = true;
    }

    /// Whether the `capped` message is due at `now`: owed, and never sent or started `CAP_WINDOW`
    /// ago or more. If it is, it is no longer owed and its start is counted.
    pub fn capped_due(&mut self, now: Instant) -> bool {
        let due = self.owed
            && self
                .last_capped
                .is_none_or(|at| now.saturating_duration_since(at) >= CAP_WINDOW);
        if due {
            self.owed = false;
            self.last_capped = Some(now);
        }
        due
    }
}

/// A class of message ranked below `detected` and `explained` messages
/// (docs/adr/0028-incident-resource-and-remediation-report.md, point 6): it shares the
/// `QUEUE_LIMIT` entries with them, is dropped first when they are full, is taken only when none
/// of them waits, has its own cap on the same `--notify-max-per-hour` and its own outcomes, never
/// owes the `capped` message and is never checked against the `detected` messages sent. The
/// classes are ranked by their order: a later one is dropped before and taken after an earlier one.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LowClass {
    Remediation,
}

#[cfg(test)]
impl LowClass {
    pub const ALL: [LowClass; 1] = [LowClass::Remediation];

    /// The line logged, at most once an hour, when a message of the class is dropped.
    fn dropped_line(self) -> &'static str {
        match self {
            LowClass::Remediation => "remediation dropped",
        }
    }

    /// The line logged, at most once an hour, when a message of the class is over its cap.
    fn capped_line(self) -> &'static str {
        match self {
            LowClass::Remediation => "remediation capped",
        }
    }
}

/// A message of a [`LowClass`].
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Low {
    /// kurita's proposal for one of kausa's Incidents.
    Remediation(crate::remediation::Proposal),
}

#[cfg(test)]
impl Low {
    pub fn class(&self) -> LowClass {
        match self {
            Low::Remediation(_) => LowClass::Remediation,
        }
    }

    /// The fingerprint of kausa's own incident the message is about.
    fn fingerprint(&self) -> &str {
        match self {
            Low::Remediation(r) => r.fingerprint(),
        }
    }
}

/// What became of a message of a [`LowClass`]: the outcomes of `kausa_notifications_total`,
/// counted apart, which the caller adds to its class's own counter.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowOutcome {
    Sent,
    Failed,
    Dropped,
    Capped,
}

#[cfg(test)]
impl LowOutcome {
    pub const ALL: [LowOutcome; 4] = [
        LowOutcome::Sent,
        LowOutcome::Failed,
        LowOutcome::Dropped,
        LowOutcome::Capped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LowOutcome::Sent => "sent",
            LowOutcome::Failed => "failed",
            LowOutcome::Dropped => "dropped",
            LowOutcome::Capped => "capped",
        }
    }
}

/// The messages of the lower classes waiting, oldest first, and what each class keeps apart: its
/// cap, its outcomes not yet taken, and when its `dropped` and `capped` lines were last logged.
#[cfg(test)]
struct Lower {
    queue: VecDeque<Low>,
    caps: [Cap; LowClass::ALL.len()],
    outcomes: [[u64; LowOutcome::ALL.len()]; LowClass::ALL.len()],
    dropped_line: [Hourly; LowClass::ALL.len()],
    capped_line: [Hourly; LowClass::ALL.len()],
}

#[cfg(test)]
impl Lower {
    fn new(max_per_hour: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            caps: std::array::from_fn(|_| Cap::new(max_per_hour)),
            outcomes: [[0; LowOutcome::ALL.len()]; LowClass::ALL.len()],
            dropped_line: [Hourly::default(); LowClass::ALL.len()],
            capped_line: [Hourly::default(); LowClass::ALL.len()],
        }
    }

    fn count(&mut self, class: LowClass, outcome: LowOutcome) {
        let count = &mut self.outcomes[class as usize][outcome as usize];
        *count = count.saturating_add(1);
    }
}

/// A line logged at most once an hour: when it was last due, and how many times it was written.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct Hourly {
    last: Option<Instant>,
    written: u64,
}

#[cfg(test)]
impl Hourly {
    /// Whether the line may be logged again at `now`, an hour or more after it last was; if it
    /// may, `now` is noted.
    fn due(&mut self, now: Instant) -> bool {
        let due = self
            .last
            .is_none_or(|at| now.saturating_duration_since(at) >= CAP_WINDOW);
        if due {
            self.last = Some(now);
        }
        due
    }

    /// Writes the line, a warning, and counts it: the count is of the lines written.
    fn warn(&mut self, message: &str, fields: Value) -> io::Result<()> {
        self.written = self.written.saturating_add(1);
        log::warn(message, fields)
    }
}

/// Whether [`Outbox::enqueue_low`] queued its message or dropped it on arrival.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowEnqueued {
    Queued,
    Dropped,
}

/// The webhook and what its messages share across terms: the queue (emptied when a term ends),
/// the table of `detected` messages sent and the cap, which live as long as the process.
pub struct Outbox<'a> {
    notifier: &'a Notifier,
    queue: RefCell<VecDeque<Queued>>,
    sent: RefCell<Sent>,
    cap: RefCell<Cap>,
    /// The messages of the lower classes, which share `QUEUE_LIMIT` with `queue`.
    #[cfg(test)]
    lower: RefCell<Lower>,
    pub metrics: &'a Metrics,
    /// The lease term: kausa notifies only while it holds (docs/adr/0019-high-availability.md).
    pub term: &'a Term,
}

impl<'a> Outbox<'a> {
    /// The outbox of `notifier`: a `detected` message is sent once per fingerprint per `ttl`, and
    /// at most `max_per_hour` start in any 60 minutes.
    pub fn new(
        notifier: &'a Notifier,
        ttl: Duration,
        max_per_hour: usize,
        metrics: &'a Metrics,
        term: &'a Term,
    ) -> Self {
        Self {
            notifier,
            queue: RefCell::new(VecDeque::new()),
            sent: RefCell::new(Sent::new(ttl)),
            cap: RefCell::new(Cap::new(max_per_hour)),
            #[cfg(test)]
            lower: RefCell::new(Lower::new(max_per_hour)),
            metrics,
            term,
        }
    }

    /// The messages waiting, oldest first.
    #[cfg(test)]
    pub fn queue(&self) -> std::cell::Ref<'_, VecDeque<Queued>> {
        self.queue.borrow()
    }

    /// Queues `item`; a `detected` message already sent in its window is not queued. When the
    /// queue is full, one message is dropped as [`QUEUE_LIMIT`] says, counted and logged, and the
    /// `capped` message is owed: a `detected` message never evicts an `explained` one, which
    /// stands for a model call already spent and is sent once, while a `detected` message's facts
    /// stay in its Event and the log (docs/adr/0027-fact-statement-for-the-detections.md).
    pub fn enqueue(&self, item: Queued) -> io::Result<()> {
        if item.explanation.is_none() && self.sent.borrow().holds(&item.fingerprint, Instant::now())
        {
            return Ok(());
        }
        // A message of a lower class makes room first; with none queued, nothing changes.
        #[cfg(test)]
        self.evict_low(None, Instant::now())?;
        let dropped = {
            let mut queue = self.queue.borrow_mut();
            if queue.len() < QUEUE_LIMIT {
                queue.push_back(item);
                None
            } else if let Some(at) = queue.iter().position(|q| q.explanation.is_none()) {
                let dropped = queue.remove(at);
                queue.push_back(item);
                dropped
            } else if item.explanation.is_none() {
                Some(item)
            } else {
                let dropped = queue.pop_front();
                queue.push_back(item);
                dropped
            }
        };
        if let Some(dropped) = dropped {
            self.cap.borrow_mut().owe();
            self.metrics
                .inc("kausa_notifications_total", &[("outcome", "dropped")]);
            log::warn(
                "notification dropped",
                json!({"fingerprint": dropped.fingerprint, "reason": "the queue is full"}),
            )?;
        }
        Ok(())
    }

    /// Notes a message that never reached the queue, dropped by another bound (the fact queue's):
    /// the `capped` message is owed.
    pub fn owe_capped(&self) {
        self.cap.borrow_mut().owe();
    }

    /// Empties the queue when a term ends; returns how many messages it held.
    pub fn clear(&self) -> usize {
        #[cfg(test)]
        self.clear_low();
        self.queue.borrow_mut().drain(..).count()
    }

    /// The dequeue rule. The `capped` message first, when it is owed and due. Then pops and
    /// drops, without counting or logging them, the `explained` messages whose `Dedup` entry
    /// (expired or not) stores the same text and is notified, and the `detected` ones sent in
    /// their window. A `detected` message over the cap is counted as `capped` and logged, and owes
    /// the `capped` message, which is returned in its place when it is due. Returns the first
    /// message to send. An evicted or replaced `Dedup` entry does not stop one.
    pub fn take_next(&self, dedup: Option<&Dedup>, now: Instant) -> io::Result<Option<Outgoing>> {
        loop {
            if self.cap.borrow_mut().capped_due(now) {
                return Ok(Some(Outgoing::Capped));
            }
            let Some(item) = self.queue.borrow_mut().pop_front() else {
                return Ok(None);
            };
            match &item.explanation {
                Some(explanation) => {
                    if dedup.is_some_and(|d| d.already_sent(&item.fingerprint, explanation)) {
                        continue;
                    }
                }
                None => {
                    if self.sent.borrow().holds(&item.fingerprint, now) {
                        continue;
                    }
                    let mut cap = self.cap.borrow_mut();
                    if !cap.admit(now) {
                        let limit = cap.limit();
                        cap.owe();
                        drop(cap);
                        self.metrics
                            .inc("kausa_notifications_total", &[("outcome", "capped")]);
                        log::warn(
                            "notification capped",
                            json!({"fingerprint": item.fingerprint, "limit": limit}),
                        )?;
                        continue;
                    }
                }
            }
            return Ok(Some(Outgoing::Message(Box::new(item))));
        }
    }

    /// Sends one message: one attempt and, after a transient failure, one more after
    /// `RETRY_DELAY`. Counts and logs the outcome; after a success, marks an `explained` message's
    /// `Dedup` entry notified (only if it still stores this text) and a `detected` one sent. Then
    /// waits until the format's least interval ([`Format::min_interval`]) has passed since the
    /// last attempt began. A failure is only logged and counted; the error is the log's. Each
    /// attempt is made only while the lease term holds; past its end the message is dropped,
    /// counted as `dropped` and not logged.
    pub async fn deliver(&self, item: Outgoing, dedup: Option<&RefCell<Dedup>>) -> io::Result<()> {
        let notifier = self.notifier;
        let limit = self.cap.borrow().limit();
        let fingerprint = match &item {
            Outgoing::Message(queued) => Some(queued.fingerprint.as_str()),
            Outgoing::Capped => None,
        };
        let mut attempt: u8 = 1;
        loop {
            if !self.term.holds() {
                self.count_dropped();
                return Ok(());
            }
            let started = Instant::now();
            let result = match &item {
                Outgoing::Message(queued) => notifier.send(&queued.notice).await,
                Outgoing::Capped => notifier.send_capped(limit).await,
            };
            match result {
                Ok(status) => {
                    self.metrics
                        .inc("kausa_notifications_total", &[("outcome", "sent")]);
                    let (namespace, owner) = match &item {
                        Outgoing::Message(queued) => {
                            match (&queued.explanation, dedup) {
                                (Some(explanation), Some(dedup)) => {
                                    dedup
                                        .borrow_mut()
                                        .mark_notified(&queued.fingerprint, explanation);
                                }
                                (Some(_), None) => {}
                                (None, _) => {
                                    self.sent
                                        .borrow_mut()
                                        .mark(&queued.fingerprint, Instant::now());
                                }
                            }
                            let n = &queued.notice;
                            (json!(n.namespace), json!({"kind": n.kind, "name": n.name}))
                        }
                        Outgoing::Capped => (Value::Null, Value::Null),
                    };
                    log::info(
                        "notification sent",
                        json!({
                            "fingerprint": fingerprint,
                            "namespace": namespace,
                            "owner": owner,
                            "format": notifier.format().name(),
                            "status": status,
                            "attempt": attempt,
                        }),
                    )?;
                }
                Err(failure) if failure.transient && attempt == 1 => {
                    // The term ended during the attempt: no retry.
                    if !self.term.holds() {
                        self.count_dropped();
                        return Ok(());
                    }
                    self.metrics.inc("kausa_notifications_retried_total", &[]);
                    log::warn(
                        "notification retried",
                        json!({
                            "fingerprint": fingerprint,
                            "reason": failure.reason,
                            "retry_in_seconds": explain::RETRY_DELAY.as_secs(),
                        }),
                    )?;
                    tokio::time::sleep(RETRY_WAIT).await;
                    attempt = 2;
                    continue;
                }
                Err(failure) => {
                    self.metrics
                        .inc("kausa_notifications_total", &[("outcome", "failed")]);
                    log::warn(
                        "notification failed",
                        json!({"fingerprint": fingerprint, "reason": failure.reason}),
                    )?;
                }
            }
            tokio::time::sleep_until(started + notifier.format().min_interval()).await;
            return Ok(());
        }
    }

    /// A message dropped because the lease term ended: counted, not logged.
    fn count_dropped(&self) {
        self.metrics
            .inc("kausa_notifications_total", &[("outcome", "dropped")]);
    }
}

/// The lower classes ([`LowClass`]): queued, taken, sent and counted apart from the `detected` and
/// `explained` messages, whose bytes, order and counters they leave as they are.
#[cfg(test)]
impl Outbox<'_> {
    /// The messages of the lower classes waiting, oldest first.
    pub fn low_queue(&self) -> Vec<Low> {
        self.lower.borrow().queue.iter().cloned().collect()
    }

    /// When both parts of the queue hold `QUEUE_LIMIT` messages, drops the oldest queued message
    /// of the lowest class queued, provided that class ranks no higher than `newcomer` (any class,
    /// for a `detected` or `explained` message, `None`). Returns whether there is room.
    fn evict_low(&self, newcomer: Option<LowClass>, now: Instant) -> io::Result<bool> {
        let mut lower = self.lower.borrow_mut();
        if self.queue.borrow().len() + lower.queue.len() < QUEUE_LIMIT {
            return Ok(true);
        }
        let Some(lowest) = lower.queue.iter().map(Low::class).max() else {
            return Ok(false);
        };
        if newcomer.is_some_and(|class| class > lowest) {
            return Ok(false);
        }
        let Some(at) = lower.queue.iter().position(|q| q.class() == lowest) else {
            return Ok(false);
        };
        let Some(dropped) = lower.queue.remove(at) else {
            return Ok(false);
        };
        Self::dropped(&mut lower, &dropped, now)?;
        Ok(true)
    }

    /// Counts and, at most once an hour per class, logs a message of a lower class dropped because
    /// the queue is full. It never owes the `capped` message.
    fn dropped(lower: &mut Lower, dropped: &Low, now: Instant) -> io::Result<()> {
        let class = dropped.class();
        lower.count(class, LowOutcome::Dropped);
        let line = &mut lower.dropped_line[class as usize];
        if line.due(now) {
            line.warn(
                class.dropped_line(),
                json!({"fingerprint": dropped.fingerprint(), "reason": "the queue is full"}),
            )?;
        }
        Ok(())
    }

    /// Queues `item` of a lower class at `now`. When the queue is full, the oldest queued message
    /// of the lowest class goes, if it ranks no higher than `item`; with none, `item` is dropped
    /// itself. It never evicts a `detected` or an `explained` message. Says which of the two
    /// became of `item`: only a queued message marks its Incident as reported.
    pub fn enqueue_low(&self, item: Low, now: Instant) -> io::Result<LowEnqueued> {
        if self.evict_low(Some(item.class()), now)? {
            self.lower.borrow_mut().queue.push_back(item);
            return Ok(LowEnqueued::Queued);
        }
        Self::dropped(&mut self.lower.borrow_mut(), &item, now)?;
        Ok(LowEnqueued::Dropped)
    }

    /// The next message of a lower class to send, only when no `detected` or `explained` message
    /// is queued: the caller takes it after [`Outbox::take_next`] has given nothing. It is the
    /// oldest of the highest class queued; one over its class's cap is counted as `capped` and, at
    /// most once an hour, logged, and the next one is tried. Nothing here owes the `capped`
    /// message.
    pub fn take_low(&self, now: Instant) -> io::Result<Option<Low>> {
        loop {
            if !self.queue.borrow().is_empty() {
                return Ok(None);
            }
            let mut lower = self.lower.borrow_mut();
            let Some(highest) = lower.queue.iter().map(Low::class).min() else {
                return Ok(None);
            };
            let Some(item) = lower
                .queue
                .iter()
                .position(|q| q.class() == highest)
                .and_then(|at| lower.queue.remove(at))
            else {
                return Ok(None);
            };
            let cap = &mut lower.caps[highest as usize];
            if cap.admit(now) {
                return Ok(Some(item));
            }
            let limit = cap.limit();
            lower.count(highest, LowOutcome::Capped);
            let line = &mut lower.capped_line[highest as usize];
            if line.due(now) {
                line.warn(
                    highest.capped_line(),
                    json!({"fingerprint": item.fingerprint(), "limit": limit}),
                )?;
            }
        }
    }

    /// Sends one message of a lower class: one attempt and, after a transient failure, one more
    /// after `RETRY_DELAY`, which is not counted apart. Counts its outcome in its class; past the
    /// end of the lease term it is dropped. Then waits until the format's least interval has
    /// passed since the last attempt began, as [`Outbox::deliver`] does.
    pub async fn deliver_low(&self, item: Low) -> io::Result<()> {
        let class = item.class();
        let mut attempt: u8 = 1;
        loop {
            if !self.term.holds() {
                self.lower.borrow_mut().count(class, LowOutcome::Dropped);
                return Ok(());
            }
            let started = Instant::now();
            let outcome = match self.notifier.send_low(&item).await {
                Ok(_) => LowOutcome::Sent,
                Err(failure) if failure.transient && attempt == 1 => {
                    if !self.term.holds() {
                        self.lower.borrow_mut().count(class, LowOutcome::Dropped);
                        return Ok(());
                    }
                    tokio::time::sleep(RETRY_WAIT).await;
                    attempt = 2;
                    continue;
                }
                Err(_) => LowOutcome::Failed,
            };
            self.lower.borrow_mut().count(class, outcome);
            tokio::time::sleep_until(started + self.notifier.format().min_interval()).await;
            return Ok(());
        }
    }

    /// Empties the lower classes' queue when a term ends: each message is counted as dropped in
    /// its class, and none in `clear`'s count, which is of `detected` and `explained` messages.
    fn clear_low(&self) {
        let mut lower = self.lower.borrow_mut();
        let cleared: Vec<LowClass> = lower.queue.drain(..).map(|q| q.class()).collect();
        for class in cleared {
            lower.count(class, LowOutcome::Dropped);
        }
    }

    /// The outcomes of the lower classes since the last call, each with its count, and resets
    /// them: the caller adds them to each class's counter.
    pub fn take_low_outcomes(&self) -> Vec<(LowClass, LowOutcome, u64)> {
        let mut lower = self.lower.borrow_mut();
        let outcomes = std::mem::take(&mut lower.outcomes);
        LowClass::ALL
            .into_iter()
            .flat_map(|class| {
                LowOutcome::ALL.into_iter().map(move |outcome| {
                    (class, outcome, outcomes[class as usize][outcome as usize])
                })
            })
            .filter(|(_, _, count)| *count > 0)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use tokio::time::Instant;

    use k8s_openapi::jiff::Timestamp;
    use reqwest::Url;
    use serde_json::Value;

    use crate::budget::Dedup;
    use crate::context::{self, Gathered};
    use crate::detect::Detection;
    use crate::incident::{Incident, IncidentKey, Signal};
    use crate::leader::Term;
    use crate::leader::hold::{self, Held};
    use crate::metrics::Metrics;
    use crate::notification::{self, ChatId, Facts, Message, Notice, parse_chat_id};
    use crate::prompt::{self, Explanation};
    use crate::record::{ContainerRecord, EventRecord, OwnerRef, PodRecord, Regarding};
    use crate::remediation::{self, Action};

    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const FINGERPRINT: &str = "5d0c2b7e9a1f4c38";

    /// A visibly fake token in the shape of a Telegram bot token, built at run time so that no
    /// secret scanner takes the source for a real one.
    fn fake_telegram_token() -> String {
        format!("{}:{}{}", "0".repeat(10), "AA", "x".repeat(33))
    }

    fn fake_bearer() -> String {
        format!("kausa-test-{}", "bearer")
    }

    fn incident() -> Incident {
        let owner = OwnerRef {
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "web".into(),
            uid: "an-invented-uid".into(),
        };
        let key = IncidentKey {
            namespace: "shop".into(),
            owner_kind: "Deployment".into(),
            owner_name: "web".into(),
            detection: Detection::CrashLoop,
        };
        let first_seen: Timestamp = "2026-10-28T11:59:00Z".parse().expect("a timestamp");
        Incident {
            sample: Signal {
                key: key.clone(),
                owner: owner.clone(),
                observed_at: first_seen,
                event: None,
                regarding_kind: None,
                pod: Some("web-7d9c8b6f4d-2xkqp".into()),
                container: Some("server".into()),
                image: None,
                reason: "CrashLoopBackOff".into(),
                message: "SAMPLE-CANARY back-off restarting failed container".into(),
                exit_code: Some(1),
            },
            key,
            owner,
            fingerprint: FINGERPRINT.into(),
            first_seen,
            last_seen: first_seen,
            pods: BTreeSet::from(["web-7d9c8b6f4d-2xkqp".to_owned()]),
        }
    }

    fn explanation(summary: &str) -> Explanation {
        Explanation {
            summary: summary.into(),
            cause: "It is started with a flag it does not know.".into(),
            steps: vec!["Check the container's args in the Deployment.".into()],
            confidence: "medium".into(),
        }
    }

    fn narrow() -> Explanation {
        explanation("The container exits right after it starts.")
    }

    fn item() -> Queued {
        queued(&incident(), 1, narrow())
    }

    fn item_for(fingerprint: &str, summary: &str) -> Queued {
        let mut incident = incident();
        incident.fingerprint = fingerprint.into();
        queued(&incident, 1, explanation(summary))
    }

    /// The `detected` message of the incident with `fingerprint`.
    fn detected_for(fingerprint: &str) -> Queued {
        let mut incident = incident();
        incident.fingerprint = fingerprint.into();
        detected(&Fact::new(&incident, fact::statement(&incident, 1).facts))
    }

    /// The explanation path's deduplication cache, with `ttl`.
    fn dedup(ttl: Duration) -> RefCell<Dedup> {
        RefCell::new(Dedup::new(ttl, 1024))
    }

    /// The outbox of `notifier`, at the default cap. Without --leader-lease the term never ends.
    fn outbox<'a>(notifier: &'a Notifier, metrics: &'a Metrics, ttl: Duration) -> Outbox<'a> {
        let term = Box::leak(Box::new(Term::unbounded()));
        Outbox::new(notifier, ttl, DEFAULT_MAX_PER_HOUR, metrics, term)
    }

    /// A notifier that never reaches anything, for the tests that only queue.
    fn nowhere() -> &'static Notifier {
        let hook = format!("http://127.0.0.1:{}/hook/x", closed_port());
        Box::leak(Box::new(notifier_at(&hook, Format::Json)))
    }

    fn notifier_at(hook: &str, format: Format) -> Notifier {
        Notifier::new(url(hook), format, None, true).expect("a notifier")
    }

    /// The next message the dequeue rule gives, which must not be the `capped` one.
    fn next(outbox: &Outbox<'_>, dedup: Option<&Dedup>) -> Option<Queued> {
        match outbox.take_next(dedup, Instant::now()).expect("the log") {
            Some(Outgoing::Message(queued)) => Some(*queued),
            Some(Outgoing::Capped) => panic!("the capped message"),
            None => None,
        }
    }

    const TTL: Duration = Duration::from_secs(6 * 3600);

    /// The value of one series in `/metrics`, 0 when it was never incremented.
    fn counter(metrics: &Metrics, series: &str) -> u64 {
        let text = metrics.render();
        let name = series.split('{').next().unwrap_or(series);
        assert!(text.contains(&format!("# TYPE {name} counter\n")), "{text}");
        text.lines()
            .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
            .map_or(0, |value| value.parse().expect("a count"))
    }

    fn capped(metrics: &Metrics) -> u64 {
        counter(metrics, "kausa_notifications_total{outcome=\"capped\"}")
    }

    fn outcomes(metrics: &Metrics) -> (u64, u64, u64, u64) {
        (
            counter(metrics, "kausa_notifications_total{outcome=\"sent\"}"),
            counter(metrics, "kausa_notifications_total{outcome=\"failed\"}"),
            counter(metrics, "kausa_notifications_total{outcome=\"dropped\"}"),
            counter(metrics, "kausa_notifications_retried_total"),
        )
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    /// One request the fake webhook received: the request line, the head in lowercase, the body
    /// and when it was read whole, by the real clock, before its answer was written.
    struct Received {
        at: std::time::Instant,
        line: String,
        head: String,
        body: String,
    }

    impl Received {
        fn json(&self) -> Value {
            serde_json::from_str(&self.body).expect("a JSON body")
        }
    }

    /// What the fake webhook does with one connection.
    enum Answer {
        /// A status line such as `200 OK`, with an empty body.
        Status(&'static str),
        /// A `302` to `location`.
        Redirect(String),
        /// Reads the request and answers nothing for this long.
        Hang(Duration),
        /// Holds the request until the test's term has ended, then answers this status line.
        Held(Held, &'static str),
    }

    /// A fake webhook on localhost: one connection per answer, in order. Each request is sent
    /// back on the channel. Returns `http://127.0.0.1:<port>`.
    fn webhook(answers: Vec<Answer>) -> (String, mpsc::Receiver<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = vec![0; 65536];
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    request.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length || n == 0 {
                            let (line, head) = text[..end].split_once("\r\n").unwrap_or_default();
                            let _ = sent.send(Received {
                                at: std::time::Instant::now(),
                                line: line.to_owned(),
                                head: head.to_ascii_lowercase(),
                                body: text[end + 4..].to_owned(),
                            });
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let response = match answer {
                    Answer::Status(status) => format!(
                        "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    ),
                    Answer::Redirect(location) => format!(
                        "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\n\
                         connection: close\r\n\r\n"
                    ),
                    Answer::Hang(time) => {
                        std::thread::sleep(time);
                        continue;
                    }
                    Answer::Held(held, status) => {
                        held.wait();
                        format!(
                            "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        )
                    }
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (base, received)
    }

    /// A local port with nothing listening on it.
    fn closed_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port()
    }

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a URL")
    }

    fn notifier(base: &str, format: Format, token: Option<String>) -> Notifier {
        Notifier::new(url(&format!("{base}/hook/x")), format, token, true).expect("a notifier")
    }

    fn telegram(chat: &str) -> Format {
        Format::Telegram(parse_chat_id(chat).expect("a chat id"))
    }

    // --- Constants and formats -------------------------------------------------------------------

    #[test]
    fn the_limits_of_the_contract() {
        assert_eq!(QUEUE_LIMIT, 16);
        assert_eq!(MIN_INTERVAL, Duration::from_secs(1));
        assert_eq!(TELEGRAM_MIN_INTERVAL, Duration::from_secs(3));
        assert_eq!(crate::explain::RETRY_DELAY, Duration::from_secs(30));
    }

    #[test]
    fn formats_have_their_names() {
        assert_eq!(Format::Json.name(), "json");
        assert_eq!(Format::Slack.name(), "slack");
        assert_eq!(telegram("-1001234567890").name(), "telegram");
    }

    #[test]
    fn telegram_waits_three_seconds_between_sends_and_the_others_one() {
        assert_eq!(Format::Json.min_interval(), Duration::from_secs(1));
        assert_eq!(Format::Slack.min_interval(), Duration::from_secs(1));
        assert_eq!(
            telegram("-1001234567890").min_interval(),
            Duration::from_secs(3)
        );
        assert_eq!(
            telegram("@kausa_alerts").min_interval(),
            Duration::from_secs(3)
        );
    }

    // --- URL checks ------------------------------------------------------------------------------

    const NOT_A_URL: &str = "is not a valid URL";
    const NOT_HTTPS: &str = "must be https:// (or http:// with --notify-allow-http)";
    const USER_INFO: &str = "must not carry a user name or password";
    const TELEGRAM_PATH: &str = "must end in /bot<token>/sendMessage with --notify-format telegram";

    #[test]
    fn a_url_must_parse() {
        for text in [
            "",
            "hooks.example.com/x",
            "https://",
            "http://[::1",
            "not a url",
            "://x",
        ] {
            for format in [Format::Json, Format::Slack, telegram("-1")] {
                assert_eq!(
                    check_url(text, &format, true).err(),
                    Some(NOT_A_URL),
                    "{text:?} as {}",
                    format.name()
                );
            }
        }
    }

    #[test]
    fn a_url_must_be_https_unless_http_is_allowed() {
        let good = "https://hooks.example.com/services/T0/B0/fake";
        assert_eq!(
            check_url(good, &Format::Json, false)
                .expect("an https URL")
                .as_str(),
            good
        );
        assert!(check_url("HTTPS://hooks.example.com/x", &Format::Slack, false).is_ok());
        let plain = "http://receiver.kausa.svc:8080/hook";
        assert_eq!(
            check_url(plain, &Format::Json, false).err(),
            Some(NOT_HTTPS)
        );
        assert!(check_url(plain, &Format::Json, true).is_ok());
        for other in [
            "ftp://hooks.example.com/x",
            "file:///etc/passwd",
            "ws://hooks.example.com/x",
            "mailto:ops@example.com",
            "data:text/plain,x",
        ] {
            for allow in [false, true] {
                assert_eq!(
                    check_url(other, &Format::Json, allow).err(),
                    Some(NOT_HTTPS),
                    "{other} with allow_http {allow}"
                );
            }
        }
    }

    #[test]
    fn a_url_must_not_carry_user_information() {
        for text in [
            "https://user:secret@hooks.example.com/x",
            "https://user@hooks.example.com/x",
            "https://:secret@hooks.example.com/x",
            "http://user:secret@receiver.kausa.svc/x",
        ] {
            assert_eq!(
                check_url(text, &Format::Json, true).err(),
                Some(USER_INFO),
                "{text}"
            );
        }
        // The scheme is checked first.
        assert_eq!(
            check_url("ftp://user:secret@hooks.example.com/x", &Format::Json, true).err(),
            Some(NOT_HTTPS)
        );
    }

    #[test]
    fn a_telegram_url_must_end_in_bot_token_send_message() {
        let token = fake_telegram_token();
        let format = telegram("-1001234567890");
        for good in [
            format!("https://api.telegram.org/bot{token}/sendMessage"),
            // A self-hosted Bot API server, and a query, which is not checked.
            format!("https://tg.example.com/bot{token}/sendMessage"),
            format!("https://tg.example.com:8443/bot{token}/sendMessage?x=1"),
            "https://api.telegram.org/bot1/sendMessage".to_owned(),
        ] {
            let checked = check_url(&good, &format, false);
            assert!(checked.is_ok(), "{good}: {checked:?}");
        }
        assert!(
            check_url(
                &format!("http://127.0.0.1:8081/bot{token}/sendMessage"),
                &format,
                true
            )
            .is_ok()
        );
        for bad in [
            "https://api.telegram.org/bot/sendMessage".to_owned(),
            format!("https://api.telegram.org/bot{token}/getMe"),
            format!("https://api.telegram.org/bot{token}/sendMessage/"),
            format!("https://api.telegram.org/bot{token}/sendmessage"),
            format!("https://api.telegram.org/prefix/bot{token}/sendMessage"),
            format!("https://api.telegram.org/bot{token}/x/sendMessage"),
            format!("https://api.telegram.org/{token}/sendMessage"),
            "https://api.telegram.org/".to_owned(),
            "https://api.telegram.org".to_owned(),
        ] {
            let error = check_url(&bad, &format, false).err();
            assert_eq!(error, Some(TELEGRAM_PATH), "{bad}");
        }
        // The other formats take any path.
        let any = format!("https://hooks.example.com/bot{token}/getMe");
        assert!(check_url(&any, &Format::Json, false).is_ok());
        assert!(check_url(&any, &Format::Slack, false).is_ok());
        // The checks before the path come first.
        let plain = format!("http://api.telegram.org/bot{token}/getMe");
        assert_eq!(check_url(&plain, &format, false).err(), Some(NOT_HTTPS));
        let user = format!("https://u:p@api.telegram.org/bot{token}/getMe");
        assert_eq!(check_url(&user, &format, false).err(), Some(USER_INFO));
    }

    #[test]
    fn the_host_is_only_the_host() {
        let token = fake_telegram_token();
        let format = telegram("@kausa_alerts");
        let telegram_url = url(&format!("https://api.telegram.org/bot{token}/sendMessage"));
        let n = Notifier::new(telegram_url, format, None, false).expect("a notifier");
        assert_eq!(n.host(), "api.telegram.org");
        let hook = url("https://hooks.example.com:8443/services/T0/B0/fake?q=1");
        let n = Notifier::new(hook, Format::Slack, None, false).expect("a notifier");
        assert_eq!(n.host(), "hooks.example.com");
    }

    // --- One attempt -----------------------------------------------------------------------------

    fn notice() -> Notice {
        item().notice
    }

    #[test]
    fn a_json_post_carries_the_body_the_type_and_the_user_agent() {
        let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
        let status = block_on(notifier(&base, Format::Json, None).send(&notice()));
        assert_eq!(status.ok(), Some(200));
        let sent = requests.recv().expect("a request");
        assert!(sent.line.starts_with("POST /hook/x "), "{}", sent.line);
        assert!(
            sent.head
                .lines()
                .any(|l| l.trim() == "content-type: application/json"),
            "{}",
            sent.head
        );
        assert!(
            sent.head.contains(&format!("user-agent: kausa/{VERSION}")),
            "{}",
            sent.head
        );
        assert!(!sent.head.contains("authorization:"), "{}", sent.head);
        assert_eq!(sent.json(), notification::json_body(&notice(), VERSION));
    }

    #[test]
    fn the_bearer_token_goes_only_with_json() {
        let token = fake_bearer();
        let (base, requests) = webhook(vec![Answer::Status("204 No Content")]);
        let status = block_on(notifier(&base, Format::Json, Some(token.clone())).send(&notice()));
        assert_eq!(status.ok(), Some(204));
        let sent = requests.recv().expect("a request");
        assert!(
            sent.head.contains(&format!(
                "authorization: bearer {}",
                token.to_ascii_lowercase()
            )),
            "{}",
            sent.head
        );
        // A token given with another format is never sent.
        for format in [Format::Slack, telegram("-1001234567890")] {
            let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
            let name = format.name();
            let path = if name == "telegram" {
                format!("{base}/bot{}/sendMessage", fake_telegram_token())
            } else {
                format!("{base}/hook/x")
            };
            let Ok(n) = Notifier::new(url(&path), format, Some(token.clone()), true) else {
                continue;
            };
            assert!(block_on(n.send(&notice())).is_ok(), "{name}");
            let sent = requests.recv().expect("a request");
            assert!(
                !sent.head.contains("authorization:"),
                "{name}: {}",
                sent.head
            );
        }
    }

    #[test]
    fn slack_and_telegram_post_their_bodies() {
        let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
        assert!(block_on(notifier(&base, Format::Slack, None).send(&notice())).is_ok());
        let sent = requests.recv().expect("a request");
        assert_eq!(sent.json(), notification::slack_body(&notice()));
        assert!(sent.head.contains("content-type: application/json"));
        assert!(sent.head.contains(&format!("user-agent: kausa/{VERSION}")));

        let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
        let path = format!("{base}/bot{}/sendMessage", fake_telegram_token());
        let n =
            Notifier::new(url(&path), telegram("-1001234567890"), None, true).expect("a notifier");
        assert!(block_on(n.send(&notice())).is_ok());
        let sent = requests.recv().expect("a request");
        assert!(sent.line.starts_with("POST /bot"), "{}", sent.line);
        let chat = ChatId::Id(-1001234567890);
        assert_eq!(sent.json(), notification::telegram_body(&notice(), &chat));
        assert!(sent.json().get("parse_mode").is_none());
    }

    #[test]
    fn statuses_are_sent_failed_or_transient() {
        for (status, code) in [
            ("200 OK", 200),
            ("202 Accepted", 202),
            ("204 No Content", 204),
        ] {
            let (base, _requests) = webhook(vec![Answer::Status(status)]);
            let result = block_on(notifier(&base, Format::Json, None).send(&notice()));
            assert_eq!(result.ok(), Some(code), "{status}");
        }
        for (status, code, transient) in [
            ("400 Bad Request", 400, false),
            ("401 Unauthorized", 401, false),
            ("403 Forbidden", 403, false),
            ("404 Not Found", 404, false),
            ("429 Too Many Requests", 429, true),
            ("500 Internal Server Error", 500, true),
            ("502 Bad Gateway", 502, true),
            ("503 Service Unavailable", 503, true),
        ] {
            let (base, _requests) = webhook(vec![Answer::Status(status)]);
            let failure =
                block_on(notifier(&base, Format::Json, None).send(&notice())).expect_err(status);
            assert_eq!(
                failure.reason,
                format!("the webhook answered {code}"),
                "{status}"
            );
            assert_eq!(failure.transient, transient, "{status}");
        }
    }

    #[test]
    fn a_redirect_is_not_followed() {
        // Where the redirect points: a listener that reports any connection.
        let target = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let location = format!("http://{}/stolen", target.local_addr().expect("an address"));
        target.set_nonblocking(true).expect("non-blocking");
        let (base, requests) = webhook(vec![Answer::Redirect(location)]);
        let failure = block_on(notifier(&base, Format::Json, None).send(&notice()))
            .expect_err("a redirect fails");
        assert_eq!(
            failure.reason,
            "a redirect is not followed: put the final URL in the file"
        );
        assert!(!failure.transient);
        assert!(requests.recv().is_ok());
        std::thread::sleep(Duration::from_millis(200));
        assert!(target.accept().is_err(), "the redirect was followed");
    }

    #[test]
    fn a_timeout_is_a_transient_failure_without_the_url() {
        let token = fake_telegram_token();
        let (base, _requests) = webhook(vec![Answer::Hang(Duration::from_secs(15))]);
        let path = format!("{base}/bot{token}/sendMessage");
        let n =
            Notifier::new(url(&path), telegram("-1001234567890"), None, true).expect("a notifier");
        let started = Instant::now();
        let failure = block_on(n.send(&notice())).expect_err("a timeout");
        assert!(
            started.elapsed() <= Duration::from_secs(12),
            "{:?}",
            started.elapsed()
        );
        assert!(failure.transient, "{failure:?}");
        assert!(
            failure.reason.starts_with("the request failed: "),
            "{failure:?}"
        );
        for text in [failure.reason.clone(), format!("{failure:?}")] {
            assert!(!text.contains(&token), "{text}");
            assert!(!text.contains("/bot"), "{text}");
            assert!(!text.contains(&path), "{text}");
        }
    }

    #[test]
    fn a_refused_connection_is_transient_and_never_shows_the_token() {
        let token = fake_telegram_token();
        let path = format!("http://127.0.0.1:{}/bot{token}/sendMessage", closed_port());
        let n =
            Notifier::new(url(&path), telegram("-1001234567890"), None, true).expect("a notifier");
        let failure = block_on(n.send(&notice())).expect_err("a refused connection");
        assert!(failure.transient, "{failure:?}");
        assert!(
            failure.reason.starts_with("the request failed: "),
            "{failure:?}"
        );
        for text in [failure.reason.clone(), format!("{failure:?}")] {
            assert!(!text.contains(&token), "{text}");
            assert!(!text.contains(&path), "{text}");
        }
        // Nor does the bearer token of a json webhook.
        let bearer = fake_bearer();
        let hook = format!("http://127.0.0.1:{}/hook/x", closed_port());
        let n = Notifier::new(url(&hook), Format::Json, Some(bearer.clone()), true)
            .expect("a notifier");
        let failure = block_on(n.send(&notice())).expect_err("a refused connection");
        assert!(!format!("{failure:?}").contains(&bearer), "{failure:?}");
        assert!(!format!("{failure:?}").contains(&hook), "{failure:?}");
    }

    #[test]
    fn check_url_errors_never_carry_the_url() {
        let token = fake_telegram_token();
        for text in [
            format!("gopher://example.com/bot{token}/sendMessage"),
            format!("https://u:{token}@api.telegram.org/bot{token}/sendMessage"),
            format!("https://api.telegram.org/bot{token}/getMe"),
            format!("not a url {token}"),
        ] {
            let error = check_url(&text, &telegram("-1"), false).expect_err("refused");
            assert!(!error.contains(&token), "{error}");
        }
    }

    // --- Queued notifications --------------------------------------------------------------------

    #[test]
    fn a_queued_notification_names_the_rule_the_object_and_its_first_sighting() {
        let item = item();
        assert_eq!(item.fingerprint, FINGERPRINT);
        assert_eq!(item.explanation, Some(narrow()));
        assert_eq!(
            item.notice,
            Notice {
                message: Message::Explained,
                detection_id: "D001".into(),
                detection_name: "crash loop".into(),
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                namespace: "shop".into(),
                name: "web".into(),
                fingerprint: FINGERPRINT.into(),
                // The same string as the incident's log line.
                first_seen: incident().first_seen.to_string(),
                // The facts of its statement, without the container.
                facts: Facts {
                    reason: Some("CrashLoopBackOff".into()),
                    exit_code: Some(1),
                    pods_seen: 1,
                    pods_seen_is_minimum: false,
                    pods_now: 1,
                    revision: None,
                },
                summary: "The container exits right after it starts.".into(),
                cause: "It is started with a flag it does not know.".into(),
                steps: vec!["Check the container's args in the Deployment.".into()],
                confidence: "medium".into(),
            }
        );
        assert_eq!(item.notice.first_seen, "2026-10-28T11:59:00Z");
        // Which makes the reference example.
        let body = notification::json_body(&item.notice, "0.2.0");
        assert_eq!(body["object"]["name"], "web");
        assert_eq!(body["first_seen"], "2026-10-28T11:59:00Z");
    }

    /// A Gathered of Deployment `web` in `shop` whose log tail, event message, Pod message,
    /// waiting message and sample message carry canaries.
    fn gathered_with_canaries() -> Gathered {
        let incident = incident();
        Gathered {
            pods: vec![PodRecord {
                namespace: "shop".into(),
                name: "web-7d9c8b6f4d-2xkqp".into(),
                uid: "uid-pod".into(),
                controller: None,
                phase: Some("Running".into()),
                unschedulable: None,
                unschedulable_reason: None,
                reason: None,
                message: Some("POD-CANARY the pod message".into()),
                failed_at: None,
                node_name: Some("worker-7".into()),
                service_account: Some("web-runner".into()),
                image_pull_secrets: 0,
                scheduling: None,
                containers: vec![ContainerRecord {
                    name: "server".into(),
                    image: Some("registry.example.com/team/web:1.4.2".into()),
                    waiting_reason: Some("CrashLoopBackOff".into()),
                    waiting_message: Some("WAITING-CANARY back-off 40s".into()),
                    last_termination_exit_code: Some(1),
                    restart_count: 4,
                    ..Default::default()
                }],
            }],
            events: vec![EventRecord {
                uid: "e1".into(),
                reason: "BackOff".into(),
                note: Some("EVENT-CANARY Back-off restarting failed container".into()),
                regarding: Regarding {
                    api_version: "v1".into(),
                    kind: "Pod".into(),
                    namespace: "shop".into(),
                    name: "web-7d9c8b6f4d-2xkqp".into(),
                    uid: None,
                },
                reporting_controller: None,
                first_seen: None,
                last_seen: None,
                count: 4,
            }],
            pods_total: 1,
            log_tail: Some("LOG-CANARY panic: unknown flag --colour\n".into()),
            incident,
            rollout: None,
        }
    }

    const CANARIES: [&str; 5] = [
        "LOG-CANARY",
        "EVENT-CANARY",
        "POD-CANARY",
        "WAITING-CANARY",
        "SAMPLE-CANARY",
    ];

    #[test]
    fn no_body_carries_the_context_or_a_name_but_the_namespace_and_the_object() {
        let gathered = gathered_with_canaries();
        let (request, redactor) = context::build(&gathered);
        // The model's answer (a fake one, which never echoes its input) names everything.
        let answer = serde_json::json!({
            "summary": "deployment-1 in namespace-1 crashes: pod-1 on node-1 exits.",
            "cause": "container-1 of registry-1/image-1:1.4.2 runs as serviceaccount-1 \
                      and reaches <redacted:ip>.",
            "steps": ["Check Deployment-1's args in namespace-1."],
            "confidence": "medium",
        })
        .to_string();
        let explanation = prompt::validate(&answer).expect("a valid answer");
        let incident = &gathered.incident;
        let names = [
            incident.key.namespace.as_str(),
            incident.owner.name.as_str(),
        ];
        let narrow = explanation.map_clean(|t| redactor.restore_only(t, &names));
        let item = queued(incident, gathered.pods_total, narrow);
        let explained = item.explanation.as_ref().expect("an explanation");
        assert_eq!(
            explained.summary,
            "web in shop crashes: pod-1 on node-1 exits."
        );
        assert_eq!(
            explained.cause,
            "container-1 of registry-1/image-1:1.4.2 runs as serviceaccount-1 and reaches \
             <redacted:ip>."
        );
        assert_eq!(explained.steps, ["Check web's args in shop."]);
        let bodies = [
            notification::json_body(&item.notice, VERSION).to_string(),
            notification::slack_body(&item.notice).to_string(),
            notification::telegram_body(&item.notice, &ChatId::Id(-1)).to_string(),
        ];
        for body in &bodies {
            for leaked in CANARIES.iter().copied().chain([
                "web-7d9c8b6f4d",
                "worker-7",
                "web-runner",
                "server",
                "registry.example.com",
                "team/web",
                "untrusted_cluster_data",
                "log_tail",
                "warning_events",
                "namespace-1",
                "deployment-1",
                "Deployment-1",
                "an-invented-uid",
                "uid-pod",
            ]) {
                assert!(!body.contains(leaked), "{leaked} in {body}");
            }
            assert!(body.contains("shop") && body.contains("web"), "{body}");
            assert!(body.contains("pod-1") && body.contains("node-1"), "{body}");
            // The sample's reason goes only as a typed fact, from its list.
            assert!(body.contains("CrashLoopBackOff"), "{body}");
        }
        // The request to the model is what carried the context, redacted.
        let request = request.to_string();
        assert!(request.contains("namespace-1"), "{request}");
    }

    // --- The queue and the dequeue rule ----------------------------------------------------------

    #[test]
    fn a_full_queue_drops_its_oldest_notification_and_counts_it() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        for i in 0..QUEUE_LIMIT {
            outbox
                .enqueue(item_for(&format!("fp-{i}"), "s"))
                .expect("the log");
        }
        assert_eq!(outbox.queue().len(), QUEUE_LIMIT);
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0));
        outbox
            .enqueue(item_for(&format!("fp-{QUEUE_LIMIT}"), "s"))
            .expect("the log");
        let queue = outbox.queue();
        assert_eq!(queue.len(), QUEUE_LIMIT);
        assert_eq!(queue.front().map(|q| q.fingerprint.as_str()), Some("fp-1"));
        assert_eq!(queue.back().map(|q| q.fingerprint.as_str()), Some("fp-16"));
        assert_eq!(outcomes(&metrics), (0, 0, 1, 0));
    }

    #[test]
    fn detected_messages_dropped_by_a_full_queue_owe_the_capped_message() {
        // More incidents settle at once than the queue holds, far below the hourly cap.
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        for i in 0..25 {
            outbox
                .enqueue(detected_for(&format!("fp-{i}")))
                .expect("the log");
        }
        assert_eq!(outbox.queue().len(), QUEUE_LIMIT);
        assert_eq!(outcomes(&metrics), (0, 0, 9, 0));
        let now = Instant::now();
        let mut taken = Vec::new();
        while let Some(next) = outbox.take_next(None, now).expect("the log") {
            taken.push(next);
        }
        // The receiver is told first that messages were not sent, then gets the newest 16.
        assert_eq!(taken.len(), 1 + QUEUE_LIMIT);
        assert_eq!(taken[0], Outgoing::Capped);
        assert_eq!(
            taken[1],
            Outgoing::Message(Box::new(detected_for("fp-9"))),
            "the oldest left first"
        );
        assert_eq!(capped(&metrics), 0, "none was over the cap");
        // Owed once: nothing more was dropped, and the next message is a queued one.
        outbox.enqueue(detected_for("fp-25")).expect("the log");
        assert_eq!(
            outbox.take_next(None, now).expect("the log"),
            Some(Outgoing::Message(Box::new(detected_for("fp-25"))))
        );
    }

    #[test]
    fn a_detected_message_never_evicts_an_explained_one() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        // A full queue of one explained message and detected ones: a new one drops the oldest
        // detected, not the explained message ahead of it.
        outbox
            .enqueue(item_for("explained-0", "s"))
            .expect("the log");
        for i in 1..QUEUE_LIMIT {
            outbox
                .enqueue(detected_for(&format!("fp-{i}")))
                .expect("the log");
        }
        outbox.enqueue(detected_for("fp-new")).expect("the log");
        {
            let queue = outbox.queue();
            assert_eq!(queue.len(), QUEUE_LIMIT);
            assert_eq!(queue[0].fingerprint, "explained-0");
            assert_eq!(queue[1].fingerprint, "fp-2");
            assert_eq!(queue[QUEUE_LIMIT - 1].fingerprint, "fp-new");
        }
        // A new explained message drops the oldest detected one too.
        outbox
            .enqueue(item_for("explained-1", "s"))
            .expect("the log");
        {
            let queue = outbox.queue();
            assert_eq!(queue[0].fingerprint, "explained-0");
            assert_eq!(queue[1].fingerprint, "fp-3");
            assert_eq!(queue[QUEUE_LIMIT - 1].fingerprint, "explained-1");
        }
        // A queue of explained messages only: a new detected message is the one dropped.
        let only = Metrics::default();
        let explained = outbox_with(&only);
        for i in 0..QUEUE_LIMIT {
            explained
                .enqueue(item_for(&format!("explained-{i}"), "s"))
                .expect("the log");
        }
        explained.enqueue(detected_for("fp-late")).expect("the log");
        assert!(
            explained
                .queue()
                .iter()
                .all(|q| q.fingerprint.starts_with("explained-"))
        );
        assert_eq!(outcomes(&only), (0, 0, 1, 0));
        // Each drop owes the capped message.
        assert_eq!(
            explained.take_next(None, Instant::now()).expect("the log"),
            Some(Outgoing::Capped)
        );
        assert_eq!(outcomes(&metrics), (0, 0, 2, 0));
    }

    /// The outbox of a notifier that reaches nothing, at the default cap.
    fn outbox_with(metrics: &Metrics) -> Outbox<'_> {
        outbox(nowhere(), metrics, TTL)
    }

    #[test]
    fn the_dequeue_rule_skips_only_what_was_sent() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        let mut dedup = Dedup::new(TTL, 1024);
        assert!(next(&outbox, Some(&dedup)).is_none());

        // Not notified yet: sent.
        dedup.put(
            FINGERPRINT,
            explanation("full"),
            Some(narrow()),
            std::time::Instant::now(),
        );
        outbox.enqueue(item()).expect("the log");
        let taken = next(&outbox, Some(&dedup)).expect("an item");
        assert_eq!(taken.fingerprint, FINGERPRINT);
        assert!(outbox.queue().is_empty());

        // Notified with the same text: two queued copies give nothing more, and the queue is
        // emptied of them.
        assert!(dedup.mark_notified(FINGERPRINT, &narrow()));
        outbox.enqueue(item()).expect("the log");
        outbox.enqueue(item()).expect("the log");
        assert!(next(&outbox, Some(&dedup)).is_none());
        assert!(outbox.queue().is_empty());

        // Duplicates in front of another notification are dropped on the way to it.
        outbox.enqueue(item()).expect("the log");
        outbox.enqueue(item_for("other", "s")).expect("the log");
        outbox.enqueue(item()).expect("the log");
        let taken = next(&outbox, Some(&dedup)).expect("an item");
        assert_eq!(taken.fingerprint, "other");
        assert_eq!(outbox.queue().len(), 1);
        assert!(next(&outbox, Some(&dedup)).is_none());

        // An entry that stores another text, even notified, does not stop it.
        outbox
            .enqueue(queued(&incident(), 1, explanation("an older text")))
            .expect("the log");
        let taken = next(&outbox, Some(&dedup)).expect("an item");
        assert_eq!(taken.explanation, Some(explanation("an older text")));

        // An evicted or expired entry does not stop it either, nor does the lack of a model.
        let empty = Dedup::new(TTL, 1024);
        outbox.enqueue(item()).expect("the log");
        assert!(next(&outbox, Some(&empty)).is_some());
        outbox.enqueue(item()).expect("the log");
        assert!(next(&outbox, None).is_some());
        // Nothing was counted.
        assert_eq!((outcomes(&metrics), capped(&metrics)), ((0, 0, 0, 0), 0));
    }

    // --- Detected messages: their facts, their table and their cap -------------------------------

    #[test]
    fn a_detected_message_carries_the_facts_and_no_text_of_a_model() {
        let item = detected_for(FINGERPRINT);
        assert_eq!(item.fingerprint, FINGERPRINT);
        assert_eq!(item.explanation, None);
        assert_eq!(
            item.notice,
            Notice {
                message: Message::Detected,
                detection_id: "D001".into(),
                detection_name: "crash loop".into(),
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                namespace: "shop".into(),
                name: "web".into(),
                fingerprint: FINGERPRINT.into(),
                first_seen: "2026-10-28T11:59:00Z".into(),
                facts: Facts {
                    reason: Some("CrashLoopBackOff".into()),
                    exit_code: Some(1),
                    pods_seen: 1,
                    pods_seen_is_minimum: false,
                    pods_now: 1,
                    revision: None,
                },
                summary: String::new(),
                cause: String::new(),
                steps: Vec::new(),
                confidence: String::new(),
            }
        );
        // The container, which the Event's statement names, never goes in a message, and neither
        // does the sample's message.
        let bodies = [
            notification::json_body(&item.notice, VERSION).to_string(),
            notification::slack_body(&item.notice).to_string(),
            notification::telegram_body(&item.notice, &ChatId::Id(-1)).to_string(),
        ];
        for body in &bodies {
            for leaked in [
                "server",
                "SAMPLE-CANARY",
                "web-7d9c8b6f4d",
                "an-invented-uid",
            ] {
                assert!(!body.contains(leaked), "{leaked} in {body}");
            }
        }
    }

    #[test]
    fn the_table_of_sent_messages_holds_each_fingerprint_for_its_window() {
        let now = Instant::now();
        let mut sent = super::Sent::new(TTL);
        assert!(!sent.holds("a", now));
        sent.mark("a", now);
        assert!(sent.holds("a", now + TTL - Duration::from_secs(1)));
        assert!(!sent.holds("a", now + TTL));
        assert!(!sent.holds("b", now));
        // Marking again moves it to the newest, with its new time.
        sent.mark("a", now + TTL);
        assert!(sent.holds("a", now + TTL));
        assert_eq!(sent.len(), 1);
        // At most SENT_CAPACITY fingerprints: one more drops the oldest.
        let mut full = super::Sent::new(TTL);
        for i in 0..=SENT_CAPACITY {
            full.mark(&format!("fp-{i}"), now);
        }
        assert_eq!(full.len(), SENT_CAPACITY);
        assert!(!full.holds("fp-0", now));
        assert!(full.holds(&format!("fp-{SENT_CAPACITY}"), now));
        // With a ttl of zero, nothing is held: every occurrence is sent.
        let mut zero = super::Sent::new(Duration::ZERO);
        zero.mark("a", now);
        assert!(!zero.holds("a", now));
        assert_eq!(zero.len(), 0);
    }

    #[test]
    fn the_cap_admits_its_limit_in_any_sixty_minutes() {
        let now = Instant::now();
        let mut cap = Cap::new(2);
        assert_eq!(cap.limit(), 2);
        assert!(cap.admit(now));
        assert!(cap.admit(now + Duration::from_secs(60)));
        assert!(!cap.admit(now + Duration::from_secs(120)));
        // The first start leaves the window after 60 minutes, the second a minute later.
        assert!(cap.admit(now + CAP_WINDOW));
        assert!(!cap.admit(now + CAP_WINDOW + Duration::from_secs(1)));
        assert!(cap.admit(now + CAP_WINDOW + Duration::from_secs(60)));
        // The capped message is due only when owed: once, then once 60 minutes later.
        assert!(!cap.capped_due(now));
        cap.owe();
        assert!(cap.capped_due(now));
        assert!(!cap.capped_due(now), "no longer owed");
        cap.owe();
        assert!(!cap.capped_due(now + CAP_WINDOW - Duration::from_secs(1)));
        assert!(cap.capped_due(now + CAP_WINDOW));
    }

    #[test]
    fn over_the_cap_a_detected_message_is_counted_and_the_capped_one_is_due_once() {
        let metrics = Metrics::default();
        let term = Term::unbounded();
        let outbox = Outbox::new(nowhere(), TTL, 2, &metrics, &term);
        for i in 0..5 {
            outbox
                .enqueue(detected_for(&format!("fp-{i}")))
                .expect("the log");
        }
        let now = Instant::now();
        let mut taken = Vec::new();
        while let Some(next) = outbox.take_next(None, now).expect("the log") {
            taken.push(next);
        }
        // Two within the cap; the third is capped and gives the capped message; the fourth and the
        // fifth are capped, and the capped message is not due again.
        assert_eq!(
            taken,
            [
                Outgoing::Message(Box::new(detected_for("fp-0"))),
                Outgoing::Message(Box::new(detected_for("fp-1"))),
                Outgoing::Capped,
            ]
        );
        assert_eq!(capped(&metrics), 3);
        assert!(outbox.queue().is_empty());
        // An explained message is not capped: it keeps the call budget's bound.
        outbox.enqueue(item()).expect("the log");
        assert_eq!(
            outbox.take_next(None, now).expect("the log"),
            Some(Outgoing::Message(Box::new(item())))
        );
        // An hour later the fourth and the fifth, capped after the capped message, owe another
        // one, which goes first; and the cap admits again.
        outbox.enqueue(detected_for("fp-5")).expect("the log");
        assert_eq!(
            outbox.take_next(None, now + CAP_WINDOW).expect("the log"),
            Some(Outgoing::Capped)
        );
        assert_eq!(
            outbox.take_next(None, now + CAP_WINDOW).expect("the log"),
            Some(Outgoing::Message(Box::new(detected_for("fp-5"))))
        );
        assert_eq!(capped(&metrics), 3);
    }

    #[test]
    fn a_detected_message_is_sent_once_per_window_and_only_after_a_2xx() {
        let (base, requests) = webhook(vec![
            Answer::Status("500 Internal Server Error"),
            Answer::Status("500 Internal Server Error"),
            Answer::Status("200 OK"),
            Answer::Status("200 OK"),
        ]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let outbox = outbox(&n, &metrics, TTL);
        let send_one = |outbox: &Outbox<'_>| {
            outbox.enqueue(detected_for(FINGERPRINT)).expect("the log");
            match outbox.take_next(None, Instant::now()).expect("the log") {
                Some(item) => {
                    block_on(outbox.deliver(item, None)).expect("the log");
                    true
                }
                None => false,
            }
        };
        // A failure (after its retry) marks nothing: the next occurrence is sent.
        assert!(send_one(&outbox));
        assert_eq!(outcomes(&metrics), (0, 1, 0, 1));
        assert!(send_one(&outbox));
        assert_eq!(outcomes(&metrics), (1, 1, 0, 1));
        // Sent: neither queued nor sent again within the window.
        assert!(!send_one(&outbox));
        assert!(outbox.queue().is_empty());
        assert_eq!(outcomes(&metrics), (1, 1, 0, 1));
        assert_eq!(requests.try_iter().count(), 3);
    }

    #[test]
    fn with_a_ttl_of_zero_every_detected_message_is_sent() {
        let (base, requests) = webhook(vec![
            Answer::Status("200 OK"),
            Answer::Status("200 OK"),
            Answer::Status("200 OK"),
        ]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let outbox = outbox(&n, &metrics, Duration::ZERO);
        block_on(async {
            for _ in 0..3 {
                outbox.enqueue(detected_for(FINGERPRINT)).expect("the log");
                let item = outbox.take_next(None, Instant::now()).expect("the log");
                outbox
                    .deliver(item.expect("not skipped"), None)
                    .await
                    .expect("the log");
            }
        });
        assert_eq!(outcomes(&metrics), (3, 0, 0, 0));
        assert_eq!(requests.try_iter().count(), 3);
    }

    #[test]
    fn the_capped_message_posts_its_fixed_body() {
        for format in ["json", "slack", "telegram"] {
            let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
            let n = match format {
                "telegram" => notifier_at(
                    &format!("{base}/bot{}/sendMessage", fake_telegram_token()),
                    telegram("-1001234567890"),
                ),
                "slack" => notifier(&base, Format::Slack, None),
                _ => notifier(&base, Format::Json, None),
            };
            let metrics = Metrics::default();
            let term = Term::unbounded();
            let outbox = Outbox::new(&n, TTL, 7, &metrics, &term);
            block_on(outbox.deliver(Outgoing::Capped, None)).expect("the log");
            let body = requests
                .recv_timeout(Duration::from_secs(1))
                .expect("a request");
            let expected = match format {
                "telegram" => notification::capped_telegram_body(7, &ChatId::Id(-1001234567890)),
                "slack" => notification::capped_slack_body(7),
                _ => notification::capped_json_body(7, VERSION),
            };
            assert_eq!(body.json(), expected, "{format}");
            assert_eq!(outcomes(&metrics), (1, 0, 0, 0), "{format}");
        }
    }

    // --- Delivery ---------------------------------------------------------------------------------

    /// Delivers `item` to a fake webhook that gives `answers`, with the Dedup entry `entry`
    /// (`Some(notice)` stored for FINGERPRINT before the delivery), and returns the metrics, the
    /// requests and the entry's `notified` afterwards (`None` without an entry).
    fn deliver_with(
        answers: Vec<Answer>,
        entry: Option<Explanation>,
        item: Queued,
    ) -> (Metrics, Vec<Received>, Option<bool>, Duration) {
        let count = answers.len();
        let (base, requests) = webhook(answers);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let (notified, elapsed) = {
            let outbox = outbox(&n, &metrics, TTL);
            let dedup = dedup(TTL);
            if let Some(notice) = entry {
                dedup.borrow_mut().put(
                    FINGERPRINT,
                    explanation("full"),
                    Some(notice),
                    std::time::Instant::now(),
                );
            }
            let started = Instant::now();
            block_on(outbox.deliver(Outgoing::Message(Box::new(item)), Some(&dedup)))
                .expect("the log");
            let elapsed = started.elapsed();
            let notified = dedup.borrow().peek(FINGERPRINT).map(|c| c.notified);
            (notified, elapsed)
        };
        let sent: Vec<Received> = (0..count)
            .filter_map(|_| requests.recv_timeout(Duration::from_millis(500)).ok())
            .collect();
        (metrics, sent, notified, elapsed)
    }

    #[test]
    fn a_2xx_is_sent_marks_its_entry_and_keeps_the_interval() {
        let (metrics, sent, notified, elapsed) =
            deliver_with(vec![Answer::Status("200 OK")], Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert_eq!(sent.len(), 1);
        assert_eq!(notified, Some(true));
        assert!(elapsed >= MIN_INTERVAL, "{elapsed:?}");
    }

    #[test]
    fn telegram_keeps_three_seconds_between_sends_and_slack_one() {
        let deliver_in = |format: &str| {
            let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
            let n = match format {
                "telegram" => {
                    let path = format!("{base}/bot{}/sendMessage", fake_telegram_token());
                    Notifier::new(url(&path), telegram("-1001234567890"), None, true)
                        .expect("a notifier")
                }
                _ => notifier(&base, Format::Slack, None),
            };
            let metrics = Metrics::default();
            let outbox = outbox(&n, &metrics, TTL);
            let started = Instant::now();
            block_on(outbox.deliver(Outgoing::Message(Box::new(item())), None)).expect("the log");
            let elapsed = started.elapsed();
            assert!(requests.recv_timeout(Duration::from_millis(500)).is_ok());
            assert_eq!(outcomes(&metrics), (1, 0, 0, 0), "{format}");
            elapsed
        };
        let elapsed = deliver_in("telegram");
        assert!(elapsed >= TELEGRAM_MIN_INTERVAL, "{elapsed:?}");
        let elapsed = deliver_in("slack");
        assert!(elapsed >= MIN_INTERVAL, "{elapsed:?}");
        assert!(elapsed < TELEGRAM_MIN_INTERVAL, "{elapsed:?}");
    }

    #[test]
    fn a_503_then_a_200_is_one_retry() {
        let answers = vec![
            Answer::Status("503 Service Unavailable"),
            Answer::Status("200 OK"),
        ];
        let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 1));
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].body, sent[1].body);
        assert_eq!(notified, Some(true));
    }

    #[test]
    fn a_429_then_a_200_is_one_retry() {
        let answers = vec![
            Answer::Status("429 Too Many Requests"),
            Answer::Status("204 No Content"),
        ];
        let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 1));
        assert_eq!(sent.len(), 2);
        assert_eq!(notified, Some(true));
    }

    #[test]
    fn two_transient_errors_fail_after_one_retry() {
        let answers = vec![
            Answer::Status("503 Service Unavailable"),
            Answer::Status("502 Bad Gateway"),
            Answer::Status("200 OK"),
        ];
        let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (0, 1, 0, 1));
        assert_eq!(sent.len(), 2, "a third attempt was made");
        assert_eq!(notified, Some(false));
    }

    #[test]
    fn a_400_or_a_redirect_fails_at_once() {
        for answer in [
            Answer::Status("400 Bad Request"),
            Answer::Status("404 Not Found"),
            Answer::Redirect("http://127.0.0.1:9/elsewhere".into()),
        ] {
            let answers = vec![answer, Answer::Status("200 OK")];
            let (metrics, sent, notified, _) = deliver_with(answers, Some(narrow()), item());
            assert_eq!(outcomes(&metrics), (0, 1, 0, 0));
            assert_eq!(sent.len(), 1, "it was tried again");
            assert_eq!(notified, Some(false));
        }
    }

    #[test]
    fn a_refused_connection_fails_after_one_retry() {
        let n = Notifier::new(
            url(&format!("http://127.0.0.1:{}/hook/x", closed_port())),
            Format::Json,
            None,
            true,
        )
        .expect("a notifier");
        let metrics = Metrics::default();
        let outbox = outbox(&n, &metrics, TTL);
        block_on(outbox.deliver(Outgoing::Message(Box::new(item())), None)).expect("the log");
        assert_eq!(outcomes(&metrics), (0, 1, 0, 1));
    }

    #[test]
    fn an_evicted_entry_is_still_sent_and_nothing_is_marked() {
        let (metrics, sent, notified, _) =
            deliver_with(vec![Answer::Status("200 OK")], None, item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert_eq!(sent.len(), 1);
        assert_eq!(notified, None);
    }

    #[test]
    fn an_older_send_does_not_mark_the_entry_that_replaced_its_own() {
        // The entry now stores a newer text than the queued notification's.
        let newer = explanation("A newer explanation.");
        let (metrics, sent, notified, _) =
            deliver_with(vec![Answer::Status("200 OK")], Some(newer), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert_eq!(sent.len(), 1);
        assert_eq!(notified, Some(false));
    }

    #[test]
    fn two_queued_notifications_for_one_entry_give_one_message() {
        let (base, requests) = webhook(vec![Answer::Status("200 OK"), Answer::Status("200 OK")]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let outbox = outbox(&n, &metrics, TTL);
        let dedup = dedup(TTL);
        dedup.borrow_mut().put(
            FINGERPRINT,
            explanation("full"),
            Some(narrow()),
            std::time::Instant::now(),
        );
        outbox.enqueue(item()).expect("the log");
        outbox.enqueue(item()).expect("the log");
        block_on(async {
            loop {
                let next = outbox
                    .take_next(Some(&dedup.borrow()), Instant::now())
                    .expect("the log");
                let Some(next) = next else { break };
                outbox.deliver(next, Some(&dedup)).await.expect("the log");
            }
        });
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert!(requests.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(requests.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn with_a_ttl_of_zero_every_sequential_occurrence_is_sent() {
        let (base, requests) = webhook(vec![
            Answer::Status("200 OK"),
            Answer::Status("200 OK"),
            Answer::Status("200 OK"),
        ]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let outbox = outbox(&n, &metrics, Duration::ZERO);
        let dedup = dedup(Duration::ZERO);
        block_on(async {
            for _ in 0..3 {
                // What explain::run does with --dedup-hours 0: no fresh entry, a model call, a
                // new entry and a queued notification.
                let now = std::time::Instant::now();
                assert!(dedup.borrow_mut().get(FINGERPRINT, now).is_none());
                dedup
                    .borrow_mut()
                    .put(FINGERPRINT, explanation("full"), Some(narrow()), now);
                outbox.enqueue(item()).expect("the log");
                let next = outbox
                    .take_next(Some(&dedup.borrow()), Instant::now())
                    .expect("the log");
                outbox
                    .deliver(next.expect("not skipped"), Some(&dedup))
                    .await
                    .expect("the log");
            }
        });
        assert_eq!(outcomes(&metrics), (3, 0, 0, 0));
        assert_eq!(requests.try_iter().count(), 3);
    }

    // Acceptance tests for the standby replica (docs/adr/0019-high-availability.md), written before
    // the code: `deliver` checks the outbox's term before each attempt, the first and the retry. Past
    // the term's end it sends nothing and the item is dropped, counted as
    // `kausa_notifications_total{outcome="dropped"}`, never as sent or failed.

    #[test]
    fn the_test_timeout_is_within_the_write_bound() {
        // The production TIMEOUT (10 s) is checked by a const assertion in the module.
        assert!(TIMEOUT <= crate::leader::pure::WRITE_BOUND);
    }

    #[test]
    fn a_closed_term_sends_nothing_and_drops_the_item() {
        let (base, requests) = webhook(vec![Answer::Status("200 OK")]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let term = Term::closed();
        {
            let outbox = Outbox::new(&n, TTL, DEFAULT_MAX_PER_HOUR, &metrics, &term);
            let dedup = dedup(TTL);
            dedup.borrow_mut().put(
                FINGERPRINT,
                explanation("full"),
                Some(narrow()),
                std::time::Instant::now(),
            );
            block_on(outbox.deliver(Outgoing::Message(Box::new(item())), Some(&dedup)))
                .expect("the log");
            let notified = dedup.borrow().peek(FINGERPRINT).map(|c| c.notified);
            assert_eq!(notified, Some(false), "a dropped item is not notified");
        }
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "the webhook was called"
        );
        let (sent, failed, dropped, _) = outcomes(&metrics);
        assert_eq!((sent, failed, dropped), (0, 0, 1));
    }

    #[test]
    fn a_term_that_ends_before_the_retry_drops_the_item() {
        // The first attempt is answered with a transient failure after the term has ended: the
        // retry is not sent.
        let (held, hold) = hold::new();
        let (base, requests) = webhook(vec![
            Answer::Held(held, "503 Service Unavailable"),
            Answer::Status("200 OK"),
        ]);
        let n = notifier(&base, Format::Json, None);
        let metrics = Metrics::default();
        let term = Term::unbounded();
        {
            let outbox = Outbox::new(&n, TTL, DEFAULT_MAX_PER_HOUR, &metrics, &term);
            block_on(async {
                let (delivered, ()) = futures::join!(
                    outbox.deliver(Outgoing::Message(Box::new(item())), None),
                    hold.end(&term)
                );
                delivered
            })
            .expect("the log");
        }
        let first = requests.recv_timeout(Duration::from_secs(1));
        assert!(
            first.is_ok(),
            "the first attempt is sent while the term holds"
        );
        assert!(
            requests.recv_timeout(Duration::from_millis(300)).is_err(),
            "the retry was sent after the term ended"
        );
        let (sent, failed, dropped, _) = outcomes(&metrics);
        assert_eq!((sent, failed, dropped), (0, 0, 1));
    }

    #[test]
    fn an_unbounded_term_sends_as_before() {
        let (metrics, sent, notified, _) =
            deliver_with(vec![Answer::Status("200 OK")], Some(narrow()), item());
        assert_eq!(outcomes(&metrics), (1, 0, 0, 0));
        assert_eq!(sent.len(), 1);
        assert_eq!(notified, Some(true));
    }

    // Acceptance test for docs/adr/0025-withhold-destructive-commands.md, written before the
    // code: notification.rs has no `crate::` import (its fuzz target includes it by path), so it
    // keeps its own copy of the notice, which must not drift from prompt.rs's.
    #[test]
    fn the_notification_notice_is_the_prompt_notice() {
        assert_eq!(notification::WITHHELD, prompt::WITHHELD);
    }

    // --- The lower classes: remediation messages ---------------------------------------------

    /// The fingerprint `label` names: `label` itself when it is one (16 lowercase hex digits),
    /// otherwise its 64-bit FNV-1a hash, since an entry takes only a fingerprint.
    fn fp(label: &str) -> String {
        if label.len() == 16
            && label
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return label.to_owned();
        }
        let hash = label.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, b| {
            (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        format!("{hash:016x}")
    }

    /// The `remediation` message, a rollback of a crash loop of `shop/web`, of the incident with
    /// the fingerprint `label` names, built from an entry as every one is.
    fn remediation_for(label: &str) -> Low {
        let entry = remediation::example_entry(Detection::CrashLoop, &fp(label));
        let at = "2026-11-24T10:00:12Z".parse().expect("a timestamp");
        Low::Remediation(remediation::Proposal::new(&entry, Action::RolloutUndo, at))
    }

    /// The `remediation` outcomes recorded since the last call.
    fn low(outbox: &Outbox<'_>) -> Vec<(LowClass, LowOutcome, u64)> {
        outbox.take_low_outcomes()
    }

    fn owed(outbox: &Outbox<'_>) -> bool {
        outbox.cap.borrow().owed
    }

    fn low_fingerprints(outbox: &Outbox<'_>) -> Vec<String> {
        outbox
            .low_queue()
            .iter()
            .map(|q| q.fingerprint().to_owned())
            .collect()
    }

    #[test]
    fn the_lower_classes_and_their_outcomes_have_their_names() {
        assert_eq!(LowClass::ALL, [LowClass::Remediation]);
        let mut sorted = LowClass::ALL;
        sorted.sort();
        assert_eq!(sorted, LowClass::ALL, "ALL is in rank order");
        assert_eq!(remediation_for("fp").class(), LowClass::Remediation);
        assert_eq!(LowClass::Remediation.dropped_line(), "remediation dropped");
        assert_eq!(LowClass::Remediation.capped_line(), "remediation capped");
        let labels: Vec<&str> = LowOutcome::ALL.iter().map(|o| o.as_str()).collect();
        assert_eq!(labels, ["sent", "failed", "dropped", "capped"]);
    }

    #[test]
    fn a_full_queue_drops_its_oldest_remediation_for_a_new_one() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        let now = Instant::now();
        for i in 0..QUEUE_LIMIT {
            outbox
                .enqueue_low(remediation_for(&format!("fp-{i}")), now)
                .expect("the log");
        }
        assert_eq!(outbox.low_queue().len(), QUEUE_LIMIT);
        assert_eq!(low(&outbox), []);
        outbox
            .enqueue_low(remediation_for(&format!("fp-{QUEUE_LIMIT}")), now)
            .expect("the log");
        let queue = low_fingerprints(&outbox);
        assert_eq!(queue.len(), QUEUE_LIMIT);
        assert_eq!(queue.first(), Some(&fp("fp-1")));
        assert_eq!(queue.last(), Some(&fp("fp-16")));
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Dropped, 1)]
        );
        assert_eq!(
            outcomes(&metrics),
            (0, 0, 0, 0),
            "not counted as a notification"
        );
        assert!(
            !owed(&outbox),
            "a dropped remediation owes no capped message"
        );
    }

    #[test]
    fn a_new_remediation_is_dropped_when_the_queue_holds_no_remediation() {
        for item in [detected_for as fn(&str) -> Queued, |fp| item_for(fp, "s")] {
            let metrics = Metrics::default();
            let outbox = outbox(nowhere(), &metrics, TTL);
            for i in 0..QUEUE_LIMIT {
                outbox.enqueue(item(&format!("fp-{i}"))).expect("the log");
            }
            outbox
                .enqueue_low(remediation_for("fp-r"), Instant::now())
                .expect("the log");
            assert!(outbox.low_queue().is_empty());
            assert_eq!(outbox.queue().len(), QUEUE_LIMIT);
            assert_eq!(
                outbox.queue().front().map(|q| q.fingerprint.as_str()),
                Some("fp-0"),
                "a remediation never evicts a detected or an explained message"
            );
            assert_eq!(
                low(&outbox),
                [(LowClass::Remediation, LowOutcome::Dropped, 1)]
            );
            assert_eq!(outcomes(&metrics), (0, 0, 0, 0));
            assert!(!owed(&outbox));
        }
    }

    #[test]
    fn a_full_queue_of_remediations_costs_a_detected_or_an_explained_message_nothing() {
        for item in [detected_for as fn(&str) -> Queued, |fp| item_for(fp, "s")] {
            let metrics = Metrics::default();
            let outbox = outbox(nowhere(), &metrics, TTL);
            let now = Instant::now();
            for i in 0..QUEUE_LIMIT {
                outbox
                    .enqueue_low(remediation_for(&format!("fp-r{i}")), now)
                    .expect("the log");
            }
            outbox.enqueue(item("fp-new")).expect("the log");
            assert_eq!(outbox.queue().len(), 1, "admitted");
            let queue = low_fingerprints(&outbox);
            assert_eq!(queue.len(), QUEUE_LIMIT - 1);
            assert_eq!(queue.first(), Some(&fp("fp-r1")), "the oldest left");
            assert_eq!(
                low(&outbox),
                [(LowClass::Remediation, LowOutcome::Dropped, 1)]
            );
            assert_eq!(outcomes(&metrics), (0, 0, 0, 0), "no notification dropped");
            assert!(!owed(&outbox), "nothing owes the capped message");
        }
    }

    #[test]
    fn the_existing_rule_applies_only_once_no_remediation_is_queued() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        for i in 0..QUEUE_LIMIT - 1 {
            outbox
                .enqueue(detected_for(&format!("fp-{i}")))
                .expect("the log");
        }
        outbox
            .enqueue_low(remediation_for("fp-r"), Instant::now())
            .expect("the log");
        // The first new message takes the remediation's place; the second evicts the oldest
        // detected message, as before, which is counted and owes the capped message.
        outbox.enqueue(detected_for("fp-a")).expect("the log");
        assert!(outbox.low_queue().is_empty());
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0));
        assert!(!owed(&outbox));
        outbox.enqueue(detected_for("fp-b")).expect("the log");
        assert_eq!(outbox.queue().len(), QUEUE_LIMIT);
        assert_eq!(
            outbox.queue().front().map(|q| q.fingerprint.as_str()),
            Some("fp-1")
        );
        assert_eq!(outcomes(&metrics), (0, 0, 1, 0));
        assert!(owed(&outbox));
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Dropped, 1)]
        );
    }

    #[test]
    fn a_remediation_is_taken_only_when_no_other_message_waits() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        let now = Instant::now();
        outbox
            .enqueue_low(remediation_for("fp-r1"), now)
            .expect("the log");
        outbox.enqueue(detected_for("fp-d")).expect("the log");
        outbox
            .enqueue_low(remediation_for("fp-r2"), now)
            .expect("the log");
        outbox.enqueue(item_for("fp-e", "s")).expect("the log");
        assert_eq!(outbox.take_low(now).expect("the log"), None);
        assert_eq!(
            next(&outbox, None).map(|q| q.fingerprint),
            Some("fp-d".to_owned())
        );
        assert_eq!(outbox.take_low(now).expect("the log"), None);
        assert_eq!(
            next(&outbox, None).map(|q| q.fingerprint),
            Some("fp-e".to_owned())
        );
        assert_eq!(next(&outbox, None), None);
        assert_eq!(
            outbox.take_low(now).expect("the log"),
            Some(remediation_for("fp-r1")),
            "the oldest first"
        );
        assert_eq!(
            outbox.take_low(now).expect("the log"),
            Some(remediation_for("fp-r2"))
        );
        assert_eq!(outbox.take_low(now).expect("the log"), None);
        assert_eq!(low(&outbox), []);
    }

    #[test]
    fn an_owed_capped_message_goes_before_a_remediation() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        let now = Instant::now();
        outbox
            .enqueue_low(remediation_for("fp-r"), now)
            .expect("the log");
        outbox.owe_capped();
        assert_eq!(
            outbox.take_next(None, now).expect("the log"),
            Some(Outgoing::Capped)
        );
        assert_eq!(outbox.take_next(None, now).expect("the log"), None);
        assert_eq!(
            outbox.take_low(now).expect("the log"),
            Some(remediation_for("fp-r"))
        );
    }

    #[test]
    fn a_remediation_is_sent_although_its_detected_message_was_sent() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        let now = Instant::now();
        outbox.sent.borrow_mut().mark(FINGERPRINT, now);
        // The detected message of that fingerprint is not queued again...
        outbox.enqueue(detected_for(FINGERPRINT)).expect("the log");
        assert!(outbox.queue().is_empty());
        // ...and its remediation is queued and taken.
        outbox
            .enqueue_low(remediation_for(FINGERPRINT), now)
            .expect("the log");
        assert_eq!(
            outbox.take_low(now).expect("the log"),
            Some(remediation_for(FINGERPRINT))
        );
    }

    #[test]
    fn remediations_have_their_own_cap_on_the_same_limit() {
        // Remediations first: their cap fills, and the detected messages' cap is untouched.
        let metrics = Metrics::default();
        let term = Term::unbounded();
        let outbox = Outbox::new(nowhere(), TTL, 2, &metrics, &term);
        let now = Instant::now();
        for i in 0..3 {
            outbox
                .enqueue_low(remediation_for(&format!("fp-r{i}")), now)
                .expect("the log");
        }
        assert!(outbox.take_low(now).expect("the log").is_some());
        assert!(outbox.take_low(now).expect("the log").is_some());
        assert_eq!(outbox.take_low(now).expect("the log"), None);
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Capped, 1)]
        );
        assert_eq!(capped(&metrics), 0);
        assert!(
            !owed(&outbox),
            "a capped remediation owes no capped message"
        );
        for i in 0..2 {
            outbox
                .enqueue(detected_for(&format!("fp-d{i}")))
                .expect("the log");
        }
        assert!(next(&outbox, None).is_some());
        assert!(next(&outbox, None).is_some());
        assert_eq!(capped(&metrics), 0);

        // Detected messages first: their cap fills, and the remediations' cap is untouched.
        let metrics = Metrics::default();
        let outbox = Outbox::new(nowhere(), TTL, 2, &metrics, &term);
        for i in 0..2 {
            outbox
                .enqueue(detected_for(&format!("fp-d{i}")))
                .expect("the log");
        }
        assert!(next(&outbox, None).is_some());
        assert!(next(&outbox, None).is_some());
        for i in 0..2 {
            outbox
                .enqueue_low(remediation_for(&format!("fp-r{i}")), now)
                .expect("the log");
        }
        assert!(outbox.take_low(now).expect("the log").is_some());
        assert!(outbox.take_low(now).expect("the log").is_some());
        assert_eq!(low(&outbox), []);
    }

    #[test]
    fn thirty_remediations_every_120_seconds_leave_no_gap_in_the_window() {
        // 0028, Not verified: one message every 120 seconds exactly, at the limit of 30 an hour.
        // A start exactly 3,600 seconds old is out of the window, so the 31st start is admitted at
        // that instant and not a millisecond before, and a second one at that instant is not.
        let metrics = Metrics::default();
        let term = Term::unbounded();
        let outbox = Outbox::new(nowhere(), TTL, 30, &metrics, &term);
        let t0 = Instant::now();
        let every = Duration::from_secs(120);
        let take = |i: u32, at: Instant| {
            outbox
                .enqueue_low(remediation_for(&format!("fp-{i}")), at)
                .expect("the log");
            outbox.take_low(at).expect("the log").is_some()
        };
        for i in 0..30 {
            assert!(take(i, t0 + every * i), "start {i}");
        }
        assert_eq!(t0 + every * 30, t0 + CAP_WINDOW);
        assert!(!take(30, t0 + CAP_WINDOW - Duration::from_millis(1)));
        assert!(take(31, t0 + CAP_WINDOW));
        assert!(!take(32, t0 + CAP_WINDOW));
        assert!(!take(
            33,
            t0 + CAP_WINDOW + every - Duration::from_millis(1)
        ));
        assert!(take(34, t0 + CAP_WINDOW + every));
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Capped, 3)]
        );
    }

    #[test]
    fn the_dropped_and_capped_lines_are_logged_at_most_once_an_hour() {
        let mut line = Hourly::default();
        let now = Instant::now();
        assert!(line.due(now));
        assert!(!line.due(now));
        assert!(!line.due(now + CAP_WINDOW - Duration::from_millis(1)));
        assert!(line.due(now + CAP_WINDOW));
        assert_eq!(line.last, Some(now + CAP_WINDOW));
        assert_eq!(line.written, 0, "only a line written is counted");

        let metrics = Metrics::default();
        let term = Term::unbounded();
        let outbox = Outbox::new(nowhere(), TTL, 1, &metrics, &term);
        let class = LowClass::Remediation as usize;
        let dropped = || outbox.lower.borrow().dropped_line[class].written;
        let capped = || outbox.lower.borrow().capped_line[class].written;
        for i in 0..QUEUE_LIMIT + 3 {
            outbox
                .enqueue_low(remediation_for(&format!("fp-{i}")), now)
                .expect("the log");
        }
        assert_eq!(dropped(), 1, "one line for three drops");
        let later = now + CAP_WINDOW - Duration::from_secs(1);
        outbox
            .enqueue_low(remediation_for("fp-later"), later)
            .expect("the log");
        assert_eq!(dropped(), 1, "none more inside the hour");
        outbox
            .enqueue_low(remediation_for("fp-hour"), now + CAP_WINDOW)
            .expect("the log");
        assert_eq!(dropped(), 2, "a second line at the hour");
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Dropped, 5)]
        );

        assert!(outbox.take_low(now).expect("the log").is_some());
        assert_eq!(capped(), 0);
        outbox
            .enqueue_low(remediation_for("fp-one"), now)
            .expect("the log");
        // The cap admits one an hour: the rest of the queue is capped at `now`, one line for all.
        assert_eq!(outbox.take_low(now).expect("the log"), None);
        assert_eq!(capped(), 1, "one line for the whole queue");
        assert_eq!(
            low(&outbox),
            [(
                LowClass::Remediation,
                LowOutcome::Capped,
                QUEUE_LIMIT as u64
            )]
        );
        outbox
            .enqueue_low(remediation_for("fp-capped"), later)
            .expect("the log");
        assert_eq!(outbox.take_low(later).expect("the log"), None);
        assert_eq!(capped(), 1, "none more inside the hour");
        // At the hour the cap admits one again, and the next one capped writes a second line.
        let hour = now + CAP_WINDOW;
        for label in ["fp-admitted", "fp-capped-again"] {
            outbox
                .enqueue_low(remediation_for(label), hour)
                .expect("the log");
        }
        assert_eq!(
            outbox.take_low(hour).expect("the log"),
            Some(remediation_for("fp-admitted"))
        );
        assert_eq!(outbox.take_low(hour).expect("the log"), None);
        assert_eq!(capped(), 2, "a second line at the hour");
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Capped, 2)]
        );
    }

    #[test]
    fn enqueue_low_says_whether_it_queued_or_dropped_the_message() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        let now = Instant::now();
        assert_eq!(
            outbox
                .enqueue_low(remediation_for("fp-r"), now)
                .expect("the log"),
            LowEnqueued::Queued
        );
        assert_eq!(outbox.low_queue(), [remediation_for("fp-r")]);
        // A remediation that evicts an older one is queued itself.
        for i in 1..QUEUE_LIMIT {
            outbox
                .enqueue(detected_for(&format!("fp-{i}")))
                .expect("the log");
        }
        assert_eq!(
            outbox
                .enqueue_low(remediation_for("fp-newer"), now)
                .expect("the log"),
            LowEnqueued::Queued
        );
        assert_eq!(outbox.low_queue(), [remediation_for("fp-newer")]);
        // With no remediation to evict, the new one is dropped on arrival.
        outbox.enqueue(detected_for("fp-last")).expect("the log");
        assert!(outbox.low_queue().is_empty());
        assert_eq!(
            outbox
                .enqueue_low(remediation_for("fp-late"), now)
                .expect("the log"),
            LowEnqueued::Dropped
        );
        assert!(outbox.low_queue().is_empty());
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Dropped, 3)]
        );
    }

    #[test]
    fn a_cleared_queue_counts_its_remediations_apart() {
        let metrics = Metrics::default();
        let outbox = outbox(nowhere(), &metrics, TTL);
        let now = Instant::now();
        outbox.enqueue(detected_for("fp-d0")).expect("the log");
        outbox.enqueue(detected_for("fp-d1")).expect("the log");
        for i in 0..3 {
            outbox
                .enqueue_low(remediation_for(&format!("fp-r{i}")), now)
                .expect("the log");
        }
        assert_eq!(
            outbox.clear(),
            2,
            "only the detected and explained messages"
        );
        assert!(outbox.low_queue().is_empty());
        assert_eq!(
            low(&outbox),
            [(LowClass::Remediation, LowOutcome::Dropped, 3)]
        );
        assert_eq!(low(&outbox), [], "taken once");
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0));
    }

    /// Delivers one remediation in `format` against a fake webhook giving `answers`; returns the
    /// metrics, the outcomes recorded and the requests received.
    fn deliver_low_with(
        answers: Vec<Answer>,
        format: Format,
        term: &Term,
    ) -> (Metrics, Vec<(LowClass, LowOutcome, u64)>, Vec<Received>) {
        let count = answers.len();
        let (base, requests) = webhook(answers);
        let n = notifier(&base, format, None);
        let metrics = Metrics::default();
        let outcomes = {
            let outbox = Outbox::new(&n, TTL, DEFAULT_MAX_PER_HOUR, &metrics, term);
            block_on(outbox.deliver_low(remediation_for(FINGERPRINT))).expect("the log");
            outbox.take_low_outcomes()
        };
        let sent: Vec<Received> = (0..count)
            .filter_map(|_| requests.recv_timeout(Duration::from_millis(500)).ok())
            .collect();
        (metrics, outcomes, sent)
    }

    #[test]
    fn a_remediation_is_posted_in_each_format() {
        let Low::Remediation(r) = remediation_for(FINGERPRINT);
        let chat = parse_chat_id("-1001234567890").expect("a chat id");
        for (format, body) in [
            (
                Format::Json,
                notification::remediation_json_body(&r, VERSION),
            ),
            (Format::Slack, notification::remediation_slack_body(&r)),
            (
                Format::Telegram(chat.clone()),
                notification::remediation_telegram_body(&r, &chat),
            ),
        ] {
            let name = format.name();
            let (metrics, low, sent) =
                deliver_low_with(vec![Answer::Status("200 OK")], format, &Term::unbounded());
            assert_eq!(sent.len(), 1, "{name}");
            assert!(sent[0].line.starts_with("POST /hook/x "), "{name}");
            assert_eq!(sent[0].json(), body, "{name}");
            assert_eq!(
                low,
                [(LowClass::Remediation, LowOutcome::Sent, 1)],
                "{name}"
            );
            assert_eq!(outcomes(&metrics), (0, 0, 0, 0), "{name}");
        }
    }

    #[test]
    fn a_remediation_is_retried_once_and_its_retry_is_not_counted() {
        let term = Term::unbounded();
        let (metrics, low, sent) = deliver_low_with(
            vec![
                Answer::Status("503 Service Unavailable"),
                Answer::Status("200 OK"),
            ],
            Format::Json,
            &term,
        );
        assert_eq!(sent.len(), 2);
        // The retry starts `RETRY_WAIT` after the failed attempt's answer, which came after its
        // request was read, not before.
        let gap = sent[1].at.duration_since(sent[0].at);
        assert!(gap >= RETRY_WAIT, "{gap:?}");
        assert_eq!(low, [(LowClass::Remediation, LowOutcome::Sent, 1)]);
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0), "no retry counted");

        let (metrics, low, sent) = deliver_low_with(
            vec![
                Answer::Status("503 Service Unavailable"),
                Answer::Status("503 Service Unavailable"),
                Answer::Status("200 OK"),
            ],
            Format::Json,
            &term,
        );
        assert_eq!(sent.len(), 2, "one retry");
        assert_eq!(low, [(LowClass::Remediation, LowOutcome::Failed, 1)]);
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0));

        let (metrics, low, sent) = deliver_low_with(
            vec![Answer::Status("400 Bad Request"), Answer::Status("200 OK")],
            Format::Json,
            &term,
        );
        assert_eq!(sent.len(), 1, "a permanent failure is not retried");
        assert_eq!(low, [(LowClass::Remediation, LowOutcome::Failed, 1)]);
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0));
    }

    #[test]
    fn the_next_send_after_a_remediation_keeps_the_least_interval() {
        // After a remediation is sent, the next message of any class starts no earlier than the
        // format's least interval after the remediation began: a detected message after one in
        // `json`, another remediation after one in `telegram`.
        for (format, interval) in [("json", MIN_INTERVAL), ("telegram", TELEGRAM_MIN_INTERVAL)] {
            let (base, requests) =
                webhook(vec![Answer::Status("200 OK"), Answer::Status("200 OK")]);
            let n = match format {
                "telegram" => {
                    let path = format!("{base}/bot{}/sendMessage", fake_telegram_token());
                    Notifier::new(url(&path), telegram("-1001234567890"), None, true)
                        .expect("a notifier")
                }
                _ => notifier(&base, Format::Json, None),
            };
            let metrics = Metrics::default();
            let outbox = outbox(&n, &metrics, TTL);
            let before = std::time::Instant::now();
            block_on(async {
                outbox
                    .deliver_low(remediation_for(FINGERPRINT))
                    .await
                    .expect("the log");
                if format == "telegram" {
                    outbox.deliver_low(remediation_for("fp-next")).await
                } else {
                    let next = Outgoing::Message(Box::new(detected_for("fp-next")));
                    outbox.deliver(next, None).await
                }
                .expect("the log");
            });
            let sent: Vec<Received> = (0..2)
                .filter_map(|_| requests.recv_timeout(Duration::from_millis(500)).ok())
                .collect();
            assert_eq!(sent.len(), 2, "{format}");
            let gap = sent[1].at.duration_since(before);
            assert!(gap >= interval, "{format}: {gap:?}");
        }
    }

    #[test]
    fn a_closed_term_drops_a_remediation() {
        let (metrics, low, sent) = deliver_low_with(
            vec![Answer::Status("200 OK")],
            Format::Json,
            &Term::closed(),
        );
        assert!(sent.is_empty(), "the webhook was called");
        assert_eq!(low, [(LowClass::Remediation, LowOutcome::Dropped, 1)]);
        assert_eq!(outcomes(&metrics), (0, 0, 0, 0));
    }
}
