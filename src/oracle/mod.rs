//! Classification oracle: enqueue questions, batch on wait.
//!
//! Callers (`search` and `read`) `submit` one classification at a time and
//! `wait` when they need the label. The planner does not hold an oracle. Grouping is over the `state` JSON the caller passed — typically a
//! listing tree or a file body. The oracle hashes that JSON; Universe does not
//! grow a digest method.

pub mod jev;
mod mock;

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

pub use jev::{JevOracle, Transport, UreqTransport};
pub use mock::HeuristicOracle;

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

/// One question to evaluate against a shared System One `state`.
///
/// `instructions` and `criteria` are JSON. A string is a one-line question.
/// An object names the fields the question text refers to, so a value that
/// comes from code is a field rather than a spliced sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    /// Shared context. Equal states share a POST.
    pub state: serde_json::Value,
    pub kind: QuestionKind,
    pub instructions: serde_json::Value,
    /// Choice options, or a Noul's `true` / `false` boundary. Null omits the field.
    pub criteria: serde_json::Value,
}

impl Classification {
    pub fn choice(
        state: serde_json::Value,
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
        state: serde_json::Value,
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

/// One answer, plus the exchange that produced it when the impl is remote.
///
/// A Choice fills `choice` and `confidence`. A Noul fills `noul` with the
/// probability the answer is yes. That probability is the whole answer: a
/// Noul has no separate confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub choice: String,
    pub confidence: f64,
    pub noul: Option<f64>,
    pub exchange: Option<OracleExchange>,
}

/// One transport round-trip. Shared across every ticket resolved by that POST.
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
/// `submit` only enqueues. `wait` flushes pending work: same-state questions
/// share a POST (chunked), independent states overlap. A later Jev impl may
/// POST from `submit` once a same-state group fills a request; not yet.
pub trait Oracle {
    fn submit(&mut self, req: Classification) -> Ticket;
    fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError>;

    fn wait_many(&mut self, tickets: &[Ticket]) -> Vec<Result<Answer, OracleError>> {
        tickets.iter().copied().map(|t| self.wait(t)).collect()
    }

    /// Usage recorded on this instance so far. Local impls stay at zero.
    fn usage(&self) -> Usage {
        Usage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
