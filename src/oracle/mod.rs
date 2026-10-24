//! Classification oracle: enqueue questions, batch on wait.
//!
//! `check` and `sweep` `submit` one classification at a time and `wait`
//! when they need the label. Grouping is over the [`State`] the caller
//! passed — typically a listing of paths or a file body.

pub(crate) mod image;
pub mod jev;

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use image::Asset;

pub use jev::{JevOracle, Transport, UreqTransport};

/// Which System One primitive a [`Classification`] asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    Choice,
    Noul,
}

impl QuestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QuestionKind::Choice => "choice",
            QuestionKind::Noul => "noul",
        }
    }
}

/// System One `state` document and the digest a batch groups on.
///
/// The digest is fixed here, from the JSON alone. [`Clone`] copies it with
/// the JSON and any image assets. An asset's reference is already in the
/// JSON, so the same image shares a digest and one POST.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    json: serde_json::Value,
    assets: Vec<Asset>,
    digest: u64,
}

impl State {
    pub fn new(json: serde_json::Value) -> Self {
        Self::with_assets(json, Vec::new())
    }

    pub(crate) fn with_assets(json: serde_json::Value, assets: Vec<Asset>) -> Self {
        let digest = digest_of(&json);
        Self {
            json,
            assets,
            digest,
        }
    }

    pub(crate) fn json(&self) -> &serde_json::Value {
        &self.json
    }

    pub(crate) fn assets(&self) -> &[Asset] {
        &self.assets
    }

    pub(crate) fn digest(&self) -> u64 {
        self.digest
    }
}

impl Deref for State {
    type Target = serde_json::Value;

    fn deref(&self) -> &Self::Target {
        &self.json
    }
}

fn digest_of(json: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    match serde_json::to_vec(json) {
        Ok(bytes) => bytes.hash(&mut hasher),
        Err(_) => 0u64.hash(&mut hasher),
    }
    hasher.finish()
}

/// One question to evaluate against a shared System One `state`.
///
/// `instructions` and `criteria` are JSON. A string is a one-line question.
/// An object names the fields the question text refers to, so a value that
/// comes from code is a field rather than a spliced sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    /// Shared context. Equal states share a POST.
    pub state: State,
    pub kind: QuestionKind,
    pub instructions: serde_json::Value,
    /// Choice options, or a Noul's `true` / `false` boundary. Null omits the field.
    pub criteria: serde_json::Value,
}

impl Classification {
    pub fn choice(
        state: State,
        instructions: impl Into<String>,
        criteria: BTreeMap<String, String>,
    ) -> Self {
        let criteria = criteria
            .into_iter()
            .map(|(label, text)| (label, serde_json::Value::String(text)))
            .collect();
        Self {
            state,
            kind: QuestionKind::Choice,
            instructions: serde_json::Value::String(instructions.into()),
            criteria: serde_json::Value::Object(criteria),
        }
    }

    pub fn noul(
        state: State,
        instructions: serde_json::Value,
        criteria: serde_json::Value,
    ) -> Self {
        Self {
            state,
            kind: QuestionKind::Noul,
            instructions,
            criteria,
        }
    }
}

/// Opaque id returned by [`Oracle::submit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket(pub(crate) u64);

/// One answer.
///
/// A Choice fills `choice` and `confidence`. A Noul fills `noul` with the
/// probability the answer is yes. That probability is the whole answer: a
/// Noul has no separate confidence. The round-trip that produced the answer
/// is logged once per POST, under [`EXCHANGE_KIND`], and names these tickets.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub choice: String,
    pub confidence: f64,
    pub noul: Option<f64>,
}

/// Log kind for one oracle POST. `tickets` are the questions that POST served.
pub const EXCHANGE_KIND: &str = "exchange";

/// One logged POST. `tickets` are the questions that POST served.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeEvent {
    pub tickets: Vec<u64>,
    pub request: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// One transport round-trip, kept once per POST.
#[derive(Debug, Clone, PartialEq)]
pub struct OracleExchange {
    pub request: serde_json::Value,
    pub response: serde_json::Value,
    pub started_at: Option<SystemTime>,
    pub ended_at: Option<SystemTime>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleError {
    Config(String),
    Http(String),
    Parse(String),
    /// Cumulative spend for this oracle hit the hard cap.
    Limit(String),
}

impl fmt::Display for OracleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OracleError::Config(s)
            | OracleError::Http(s)
            | OracleError::Parse(s)
            | OracleError::Limit(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for OracleError {}

/// One timed transport round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flight {
    pub started: SystemTime,
    pub ended: SystemTime,
}

/// [`Usage`] as a log record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub spend_usd: f64,
    pub input_tokens: u64,
    pub requests: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_factor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p95_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p99_ms: Option<u64>,
}

impl UsageRecord {
    pub fn from_usage(usage: &Usage) -> Self {
        let (p50, p95, p99) = usage
            .latency
            .map(|lat| {
                (
                    Some(lat.p50.as_millis() as u64),
                    Some(lat.p95.as_millis() as u64),
                    Some(lat.p99.as_millis() as u64),
                )
            })
            .unwrap_or((None, None, None));
        Self {
            spend_usd: usage.spend_usd,
            input_tokens: usage.input_tokens,
            requests: usage.requests,
            batch_size: usage.batch_size,
            fill_factor: usage.fill_factor,
            latency_p50_ms: p50,
            latency_p95_ms: p95,
            latency_p99_ms: p99,
        }
    }
}

/// Cumulative usage for one oracle instance. Local impls stay at zero.
///
/// [`Usage::batch_size`] is mean questions-per-request / the impl's max
/// questions per request. [`Usage::fill_factor`] is mean billed input tokens /
/// the impl's max context per request. Batching should raise batch size, raise
/// fill, and cut [`Usage::requests`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub spend_usd: f64,
    pub input_tokens: u64,
    /// Completed transport round-trips.
    pub requests: u64,
    /// Mean occupancy of the per-request question budget. None if no requests.
    pub batch_size: Option<f64>,
    /// Mean billed input tokens / the impl's per-request context cap.
    /// None if no token counts.
    pub fill_factor: Option<f64>,
    /// Client round-trip wall time, including retries. None if no timed requests.
    pub latency: Option<LatencyPercentiles>,
}

/// Linear interpolations of recorded request durations at p50 / p95 / p99.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyPercentiles {
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

fn latency_percentiles(samples: &[Duration]) -> Option<LatencyPercentiles> {
    if samples.is_empty() {
        return None;
    }
    let mut nanos: Vec<u128> = samples.iter().map(|d| d.as_nanos()).collect();
    nanos.sort_unstable();
    Some(LatencyPercentiles {
        p50: duration_from_nanos(percentile_nanos(&nanos, 50.0)),
        p95: duration_from_nanos(percentile_nanos(&nanos, 95.0)),
        p99: duration_from_nanos(percentile_nanos(&nanos, 99.0)),
    })
}

fn percentile_nanos(sorted: &[u128], p: f64) -> u128 {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let rank = (p / 100.0) * (n - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    if lo == hi {
        sorted[lo]
    } else {
        let w = rank - lo as f64;
        let a = sorted[lo] as f64;
        let b = sorted[hi] as f64;
        (a + (b - a) * w).round() as u128
    }
}

fn duration_from_nanos(nanos: u128) -> Duration {
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

/// Classification oracle. Jev is the first implementation.
///
/// `submit` only enqueues. `wait` starts pending work: same-state questions
/// share a POST, and independent states overlap up to the in-flight cap.
/// A finished POST frees its slot. `wait` returns when its own ticket is
/// done, while other posts keep running.
pub trait Oracle {
    fn submit(&mut self, req: Classification) -> Ticket;
    fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError>;

    /// Block until any of `tickets` has an answer, and return that ticket.
    ///
    /// Sync oracles answer the whole queued batch inside this call. A live
    /// oracle returns when the first of them finishes and leaves the rest
    /// running.
    fn wait_any(&mut self, tickets: &[Ticket]) -> Result<Ticket, OracleError> {
        let Some(ticket) = tickets.first().copied() else {
            return Err(OracleError::Parse("no tickets to wait for".into()));
        };
        self.wait(ticket)?;
        Ok(ticket)
    }

    /// Which of `tickets` already have answers.
    ///
    /// The default reports all of them: a sync `wait` / `wait_any` finishes
    /// every question that was queued. A pipelined oracle reports only the
    /// posts that have actually returned.
    fn completed(&self, tickets: &[Ticket]) -> Vec<Ticket> {
        tickets.to_vec()
    }

    fn wait_many(&mut self, tickets: &[Ticket]) -> Vec<Result<Answer, OracleError>> {
        tickets.iter().copied().map(|t| self.wait(t)).collect()
    }

    /// Usage recorded on this instance so far. Local impls stay at zero.
    fn usage(&self) -> Usage {
        Usage::default()
    }

    /// Timed round-trips so far. Empty when the oracle does not time them.
    fn flights(&self) -> &[Flight] {
        &[]
    }

    /// Cap on overlapping round-trips. None when this oracle does not overlap.
    fn flight_limit(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_clone_keeps_the_digest() {
        let state = State::new(serde_json::json!({"tree": {"src": ["a.rs"]}}));
        let cloned = state.clone();
        assert_eq!(state.digest(), cloned.digest());
        assert_eq!(
            state.digest(),
            State::new(serde_json::json!({"tree": {"src": ["a.rs"]}})).digest()
        );
        assert_ne!(
            state.digest(),
            State::new(serde_json::json!({"tree": {"src": ["b.rs"]}})).digest()
        );
    }

    #[test]
    fn tickets_are_copy_ids() {
        let a = Ticket(0);
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, Ticket(1));
    }

    #[test]
    fn latency_percentiles_none_when_empty() {
        assert_eq!(latency_percentiles(&[]), None);
    }

    #[test]
    fn latency_percentiles_single_sample() {
        let p = latency_percentiles(&[Duration::from_millis(12)]).unwrap();
        assert_eq!(p.p50, Duration::from_millis(12));
        assert_eq!(p.p95, Duration::from_millis(12));
        assert_eq!(p.p99, Duration::from_millis(12));
    }

    #[test]
    fn latency_percentiles_interpolates() {
        let samples = [
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(30),
            Duration::from_millis(40),
            Duration::from_millis(50),
        ];
        let p = latency_percentiles(&samples).unwrap();
        assert_eq!(p.p50, Duration::from_millis(30));
        assert_eq!(p.p95, Duration::from_millis(48));
        assert_eq!(p.p99, Duration::from_nanos(49_600_000));
        assert!(p.p50 <= p.p95 && p.p95 <= p.p99);
    }
}
