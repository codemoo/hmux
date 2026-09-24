//! Pure, bounded JSONL activity primitives. The Home owner reads complete lines,
//! supplies wall time and calendar dates, and serializes access to each tracker.
//!
//! `parse_line` has no filesystem or clock access. The future Home poller must
//! enforce the 8 MiB line limit while reading, maintain bounded per-file
//! offsets, and avoid replaying old lines. A missing or invalid timestamp is
//! represented as `None`; the caller passes the same `now_day` and
//! `event_day` to `ingest` for that case. For valid timestamps, the caller
//! computes both dates in its configured local timezone, including the
//! timestamp's own UTC offset on daylight saving transitions. The tracker
//! accepts a future event within five minutes, clipping it to `now`; its
//! `event_day` argument is then ignored. One tracker belongs to one provider.
//!
//! Go parity covers ordinary values below the caps. Oversized lines, paths,
//! session IDs and model names are rejected or omitted here; Go has no such
//! parser bounds. Duplicate typed fields and deeply nested unknown JSON may
//! reject more strictly than Go. Integer overflow saturates here. At capacity the daily
//! session count and window component of the rate undercount, signaled by
//! `sessions_capped` and `window_events_dropped`; daily token totals still
//! include accepted events. Unknown JSON and selected malformed scalar trees
//! are skipped without materialization; no transcript body is retained.
use crate::Provider;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, VecDeque};
use std::fmt;

pub const MAX_LINE_BYTES: usize = 8 << 20;
pub const MAX_PATH_BYTES: usize = 2048;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_WINDOW_EVENTS: usize = 4096;
pub const MAX_DAY_SESSIONS: usize = 1024;
const WINDOW_SECONDS: i64 = 60;
const EWMA_SECONDS: f64 = 20.0;
const FUTURE_SKEW_SECONDS: i64 = 300;

fn elapsed_seconds(now: DateTime<Utc>, last: DateTime<Utc>) -> f64 {
    match (now - last).num_microseconds() {
        Some(micros) => micros as f64 / 1e6,
        None if now < last => -f64::MAX,
        None => f64::MAX,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActivitySource {
    Jsonl,
    Hermes,
}
impl ActivitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jsonl => "jsonl",
            Self::Hermes => "hermes",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TokenEvent {
    pub provider: Provider,
    /// Missing or malformed JSON timestamp: the tracker uses its explicit now.
    pub timestamp: Option<DateTime<Utc>>,
    pub tokens: i64,
    pub model: String,
    pub session_key: String,
    pub account_number: i64,
    pub source: Option<ActivitySource>,
}
impl fmt::Debug for TokenEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenEvent")
            .field("provider", &self.provider)
            .field("timestamp", &self.timestamp)
            .field("tokens", &self.tokens)
            .field("account_number", &self.account_number)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

fn bounded_string(v: Option<&RawValue>, max: usize) -> Option<String> {
    let raw = v?.get();
    // JSON escapes can use six source bytes per resulting byte. Check the raw
    // slice before asking serde_json to allocate an unescaped string.
    if raw.len() > max.saturating_mul(6).saturating_add(2) || !raw.starts_with('"') {
        return None;
    }
    let value: String = serde_json::from_str(raw).ok()?;
    (value.len() <= max).then_some(value)
}
fn number(v: Option<&RawValue>) -> i64 {
    let Some(v) = v else { return 0 };
    let raw = v.get();
    if raw.len() > 128 {
        return 0;
    }
    if let Ok(n) = raw.parse::<i64>() {
        return n;
    }
    if let Ok(f) = raw.parse::<f64>() {
        return if f.is_finite() { f as i64 } else { 0 };
    }
    0
}
// Unknown fields, including transcript content, are skipped by serde rather
// than materialized in a generic JSON tree.
#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<&'a RawValue>,
    #[serde(borrow)]
    timestamp: Option<&'a RawValue>,
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<&'a RawValue>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct ClaudeMessage<'a> {
    #[serde(borrow)]
    model: Option<&'a RawValue>,
    #[serde(borrow)]
    usage: Option<ClaudeUsage<'a>>,
}
#[derive(Deserialize)]
struct ClaudeUsage<'a> {
    #[serde(borrow)]
    input_tokens: Option<&'a RawValue>,
    #[serde(borrow)]
    output_tokens: Option<&'a RawValue>,
    #[serde(borrow)]
    cache_creation_input_tokens: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct CodexPayload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<&'a RawValue>,
    #[serde(borrow)]
    info: Option<CodexInfo<'a>>,
}
#[derive(Deserialize)]
struct CodexInfo<'a> {
    #[serde(borrow)]
    last_token_usage: Option<CodexUsage<'a>>,
}
#[derive(Deserialize)]
struct CodexUsage<'a> {
    #[serde(borrow)]
    input_tokens: Option<&'a RawValue>,
    #[serde(borrow)]
    cached_input_tokens: Option<&'a RawValue>,
    #[serde(borrow)]
    output_tokens: Option<&'a RawValue>,
}
/// Parses one complete line. The caller must bound the line before allocating it;
/// this guard also rejects oversized input. No transcript body is retained.
/// Go's missing-timestamp fallback is deferred to `Tracker::ingest`.
pub fn parse_line(provider: Provider, raw: &[u8], path: &str) -> Option<TokenEvent> {
    if raw.len() > MAX_LINE_BYTES || path.len() > MAX_PATH_BYTES {
        return None;
    }
    let obj: Line<'_> = serde_json::from_slice(raw).ok()?;
    let kind = bounded_string(obj.kind, 32)?;
    let (tokens, model, session_key) = match provider {
        Provider::Claude if kind == "assistant" => {
            let msg: ClaudeMessage<'_> = serde_json::from_str(obj.message?.get()).ok()?;
            let usage = msg.usage.as_ref()?;
            let tokens = number(usage.input_tokens)
                .saturating_add(number(usage.output_tokens))
                .saturating_add(number(usage.cache_creation_input_tokens));
            let model = bounded_string(msg.model, MAX_LABEL_BYTES).unwrap_or_default();
            let session = bounded_string(obj.session_id, MAX_LABEL_BYTES)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| path.to_owned());
            (tokens, model, session)
        }
        Provider::Codex if kind == "event_msg" => {
            let payload: CodexPayload<'_> = serde_json::from_str(obj.payload?.get()).ok()?;
            if bounded_string(payload.kind, 32)? != "token_count" {
                return None;
            }
            let info = payload.info.as_ref()?;
            let last = info.last_token_usage.as_ref();
            let fresh = number(last.and_then(|m| m.input_tokens))
                .saturating_sub(number(last.and_then(|m| m.cached_input_tokens)))
                .max(0);
            (
                fresh.saturating_add(number(last.and_then(|m| m.output_tokens))),
                String::new(),
                path.to_owned(),
            )
        }
        _ => return None,
    };
    if tokens <= 0 {
        return None;
    }
    let timestamp = bounded_string(obj.timestamp, 128)
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|t| t.with_timezone(&Utc));
    Some(TokenEvent {
        provider,
        timestamp,
        tokens,
        model,
        session_key,
        account_number: 0,
        source: None,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BurnState {
    #[default]
    Idle,
    Walk,
    Jog,
    Run,
    Fly,
    Rocket,
}
impl BurnState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Walk => "walk",
            Self::Jog => "jog",
            Self::Run => "run",
            Self::Fly => "fly",
            Self::Rocket => "rocket",
        }
    }
    fn index(self) -> usize {
        self as usize
    }
    fn from_index(i: usize) -> Self {
        [
            Self::Idle,
            Self::Walk,
            Self::Jog,
            Self::Run,
            Self::Fly,
            Self::Rocket,
        ][i.min(5)]
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct ActivitySnapshot {
    pub rate_per_minute: f64,
    pub state: BurnState,
    pub today_total_tokens: i64,
    pub today_sessions_count: usize,
    pub has_observed: bool,
    pub activity_sources: Vec<ActivitySource>,
    /// The day count and sliding-window component become lower bounds at these
    /// limits. The EWMA still observes every accepted event.
    pub sessions_capped: bool,
    pub window_events_dropped: u64,
    pub total_saturated: bool,
}
struct TimedTokens {
    at: DateTime<Utc>,
    tokens: i64,
}
/// Single-owner deterministic tracker. It never retains a provider line or model.
pub struct Tracker {
    window: VecDeque<TimedTokens>,
    ewma: f64,
    last_ewma_update: Option<DateTime<Utc>>,
    today_day: Option<NaiveDate>,
    today_total: i64,
    // Only cardinality is needed: bounded digests avoid retaining paths/IDs.
    sessions: BTreeSet<[u8; 32]>,
    sources: [bool; 2],
    has_observed: bool,
    state: BurnState,
    last_state_change: Option<DateTime<Utc>>,
    sessions_capped: bool,
    window_events_dropped: u64,
    total_saturated: bool,
}
impl Tracker {
    pub fn new(_clock: DateTime<Utc>) -> Self {
        Self {
            window: VecDeque::new(),
            ewma: 0.0,
            last_ewma_update: None,
            today_day: None,
            today_total: 0,
            sessions: BTreeSet::new(),
            sources: [false; 2],
            has_observed: false,
            state: BurnState::Idle,
            // No arithmetic below Chrono's minimum date. An unseen tracker
            // may transition immediately, matching Go's one-hour anchor.
            last_state_change: None,
            sessions_capped: false,
            window_events_dropped: 0,
            total_saturated: false,
        }
    }
    fn rollover(&mut self, day: NaiveDate) {
        if self.today_day.is_none_or(|old| day > old) {
            self.today_day = Some(day);
            self.today_total = 0;
            self.sessions.clear();
            self.sources = [false; 2];
            self.sessions_capped = false;
            self.total_saturated = false;
        }
    }
    fn evict(&mut self, now: DateTime<Utc>) {
        let cutoff = Self::cutoff(now);
        self.window.retain(|e| e.at >= cutoff && e.at <= now);
    }
    fn cutoff(now: DateTime<Utc>) -> DateTime<Utc> {
        now.checked_sub_signed(chrono::Duration::seconds(WINDOW_SECONDS))
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
    }
    fn decay(&mut self, now: DateTime<Utc>) {
        if let Some(last) = self.last_ewma_update {
            let dt = elapsed_seconds(now, last).max(0.0);
            self.ewma *= (-dt / EWMA_SECONDS).exp();
            self.last_ewma_update = Some(now);
        }
    }
    fn current_value(&self) -> f64 {
        let sliding = self
            .window
            .iter()
            .fold(0_i64, |n, e| n.saturating_add(e.tokens)) as f64;
        self.ewma.max(sliding)
    }
    fn change_state(&mut self, value: f64, now: DateTime<Utc>) {
        if let Some(last) = self.last_state_change {
            let elapsed = now - last;
            if elapsed >= chrono::Duration::zero() && elapsed < chrono::Duration::seconds(5) {
                return;
            }
        }
        let upper = [500.0, 3000.0, 12000.0, 40000.0, 100000.0];
        let lower = [400.0, 2400.0, 9600.0, 32000.0, 80000.0];
        let mut idx = self.state.index();
        while idx < 5 && value >= upper[idx] {
            idx += 1;
        }
        if idx == self.state.index() {
            while idx > 0 && value < lower[idx - 1] {
                idx -= 1;
            }
        }
        let next = BurnState::from_index(idx);
        if next != self.state {
            self.state = next;
            self.last_state_change = Some(now);
        }
    }
    fn make_snapshot(&self, value: f64) -> ActivitySnapshot {
        let mut sources = Vec::new();
        if self.sources[1] {
            sources.push(ActivitySource::Hermes);
        }
        if self.sources[0] {
            sources.push(ActivitySource::Jsonl);
        }
        ActivitySnapshot {
            rate_per_minute: value,
            state: self.state,
            today_total_tokens: self.today_total,
            today_sessions_count: self.sessions.len(),
            has_observed: self.has_observed,
            activity_sources: sources,
            sessions_capped: self.sessions_capped,
            window_events_dropped: self.window_events_dropped,
            total_saturated: self.total_saturated,
        }
    }
    fn finish(&mut self, now: DateTime<Utc>) -> ActivitySnapshot {
        self.evict(now);
        self.decay(now);
        let value = self.current_value();
        self.change_state(value, now);
        self.make_snapshot(value)
    }
    /// `now_day` and `event_day` are local calendar dates from the runtime's
    /// configured timezone. For absent timestamps pass `now_day` as event_day.
    pub fn ingest(
        &mut self,
        event: &TokenEvent,
        now: DateTime<Utc>,
        now_day: NaiveDate,
        event_day: NaiveDate,
    ) -> ActivitySnapshot {
        self.rollover(now_day);
        let at = event.timestamp.unwrap_or(now);
        if event.tokens < 0
            || at
                > now
                    .checked_add_signed(chrono::Duration::seconds(FUTURE_SKEW_SECONDS))
                    .unwrap_or(DateTime::<Utc>::MAX_UTC)
        {
            return self.finish(now);
        }
        let event_time = at.min(now);
        self.has_observed = true;
        let local_day = if at > now { now_day } else { event_day };
        if Some(local_day) == self.today_day {
            let sum = self.today_total.saturating_add(event.tokens);
            if sum == i64::MAX && event.tokens > i64::MAX - self.today_total {
                self.total_saturated = true;
            }
            self.today_total = sum;
            let session_digest: [u8; 32] = Sha256::digest(event.session_key.as_bytes()).into();
            if !self.sessions.contains(&session_digest) {
                if event.session_key.len() <= MAX_PATH_BYTES
                    && self.sessions.len() < MAX_DAY_SESSIONS
                {
                    self.sessions.insert(session_digest);
                } else {
                    self.sessions_capped = true;
                }
            }
            if let Some(source) = event.source {
                self.sources[match source {
                    ActivitySource::Jsonl => 0,
                    ActivitySource::Hermes => 1,
                }] = true;
            }
        }
        let contributes = event_time >= Self::cutoff(now);
        if contributes {
            self.evict(now);
            if self.window.len() < MAX_WINDOW_EVENTS {
                let pos = self
                    .window
                    .iter()
                    .position(|e| e.at > event_time)
                    .unwrap_or(self.window.len());
                self.window.insert(
                    pos,
                    TimedTokens {
                        at: event_time,
                        tokens: event.tokens,
                    },
                );
            } else {
                self.window_events_dropped = self.window_events_dropped.saturating_add(1);
            }
        }
        self.evict(now);
        if contributes {
            let instant = event.tokens as f64;
            if let Some(last) = self.last_ewma_update {
                let dt = elapsed_seconds(now, last).max(0.001);
                let decay = (-dt / EWMA_SECONDS).exp();
                let decayed = self.ewma * decay;
                self.ewma = decayed + (1.0 - decay) * (instant - decayed);
            } else {
                self.ewma = instant;
            }
            self.last_ewma_update = Some(now);
        } else {
            self.decay(now);
        }
        let value = self.current_value();
        self.change_state(value, now);
        self.make_snapshot(value)
    }
    pub fn snapshot(&mut self, now: DateTime<Utc>, now_day: NaiveDate) -> ActivitySnapshot {
        self.rollover(now_day);
        self.finish(now)
    }
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
